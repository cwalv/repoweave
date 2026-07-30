//! Tests for snapshot reads (§6 of the sync design doc).
//!
//! Acceptance criteria:
//!
//!   - No working-tree reads of source manifest/lock remain in the engine:
//!     verified by the `sync_reads_committed_lock_not_working_tree` test, which
//!     places a stale (uncommitted) lock version in the source's working tree
//!     and confirms sync converges to the committed version.
//!
//!   - A test that mutates the source mid-op and shows the sync result is
//!     provably source-as-of-T0, not a blend: verified by
//!     `sync_result_is_source_as_of_t0_not_working_tree_mutation`, which
//!     modifies the source lock file on disk (without committing) before
//!     running sync and confirms the destination sees the committed state.
//!
//!   - `read_file_at_revision` is tested directly in `vcs_test.rs`; here we
//!     test the sync-level contract.

use std::path::Path;

mod common;

// ---------------------------------------------------------------------------
// Helpers (mirrored from e2e_sync_abort_test.rs to keep tests self-contained)
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
        "git {:?} in {} failed:\nstdout: {}\nstderr: {}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
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
    std::fs::write(project_dir.join("rwv.yaml"), yaml).unwrap();
}

fn write_lock(project_dir: &Path, repos: &[(&str, &str, &str)]) {
    let mut yaml = String::from("repositories:\n");
    for (path, url, sha) in repos {
        yaml.push_str(&format!(
            "  {path}:\n    type: git\n    url: {url}\n    version: {sha}\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.lock"), yaml).unwrap();
}

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Sync reads the COMMITTED lock, not the working-tree lock.
///
/// Setup:
///   - source: project repo with rwv.lock committed at revision V1 of `lib`
///   - After committing V1, write V2 to the source's rwv.lock on disk WITHOUT
///     committing it (simulates a working-tree mutation that happened between
///     commits, or a crash before commit)
///   - destination: project repo cloned from source (shares history)
///
/// Expected: destination's `lib` converges to V1 (the committed lock SHA),
/// not V2 (the working-tree SHA).
///
/// Before this fix: sync would have read V2 from the working tree and converged
/// to the wrong SHA. After this fix: sync reads the committed lock at the pinned
/// revision and converges to V1.
#[test]
fn sync_reads_committed_lock_not_working_tree() {
    let tmp = common::tempdir().unwrap();
    let tmp = tmp.path();

    // ---- Build the `lib` repo with two commits ----
    let lib_path = tmp.join("lib");
    let sha_v1 = init_repo(&lib_path);
    let sha_v2 = make_commit(&lib_path, "v2.txt", "second commit\n", "feat: v2");

    // ---- Set up source workspace ----
    let source_ws = tmp.join("source");
    let source_proj = source_ws.join("projects").join("myproject");
    std::fs::create_dir_all(&source_proj).unwrap();
    init_repo(&source_proj);

    // Write manifest pointing at lib.
    let lib_url = format!("file://{}", lib_path.display());
    write_manifest(&source_proj, &[("lib", &lib_url)]);
    // Write .gitattributes for replay exclusion.
    std::fs::write(
        source_proj.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    git(&["add", "rwv.yaml", ".gitattributes"], &source_proj);
    git(&["commit", "-m", "chore: manifest + attrs"], &source_proj);

    // Materialise lib under source (clone) at sha_v1.
    let source_lib = source_ws.join("lib");
    git(&["clone", lib_url.as_str(), "lib"], &source_ws);
    git(&["checkout", &sha_v1], &source_lib);

    // Write and commit the lock at V1 (the COMMITTED lock of record).
    write_lock(&source_proj, &[("lib", lib_url.as_str(), &sha_v1)]);
    git(&["add", "rwv.lock"], &source_proj);
    git(&["commit", "-m", "lock: pin lib at v1"], &source_proj);

    // Write .rwv-active for source workspace so WorkspaceContext::resolve works.
    std::fs::write(source_ws.join(".rwv-active"), "myproject\n").unwrap();

    // ------------------------------------------------------------------
    // KEY MUTATION: after committing the lock at V1, overwrite the
    // working-tree lock with V2. This is the "source mid-mutation"
    // scenario. The working tree now disagrees with the committed lock.
    // ------------------------------------------------------------------
    write_lock(&source_proj, &[("lib", lib_url.as_str(), &sha_v2)]);
    // Deliberately do NOT commit — the working tree has the stale V2 lock.

    // ---- Set up destination workspace (clone of source project) ----
    let dest_ws = tmp.join("destination");
    let dest_projects = dest_ws.join("projects");
    std::fs::create_dir_all(&dest_projects).unwrap();
    // Clone source project into dest.
    git(
        &["clone", source_proj.to_str().unwrap(), "myproject"],
        &dest_projects,
    );
    let dest_proj = dest_projects.join("myproject");

    // Clone lib under destination at sha_v1.
    let dest_lib = dest_ws.join("lib");
    git(&["clone", lib_url.as_str(), "lib"], &dest_ws);
    git(&["checkout", &sha_v1], &dest_lib);

    // Write .rwv-active so sync can find the active project without --project.
    std::fs::write(dest_ws.join(".rwv-active"), "myproject\n").unwrap();

    // ---- Run sync from source ----
    // FF sync: destination's project tip equals source's committed tip
    // (cloned), so Phase 1' is a no-op. Phase 2 reads the COMMITTED
    // lock (V1) and should converge lib to sha_v1.
    rwv()
        .current_dir(&dest_proj)
        .args(["sync", source_proj.to_str().unwrap(), "--strategy=ff"])
        .assert()
        .success();

    // ---- Assert: lib converged to V1 (committed SHA), not V2 ----
    let actual_lib_head = git_out(&["rev-parse", "HEAD"], &dest_lib);
    assert_eq!(
        actual_lib_head, sha_v1,
        "Expected dest lib HEAD to be sha_v1 ({sha_v1}), got {actual_lib_head}.\n\
         If this fails, sync is reading the working-tree lock (V2) instead of \
         the committed lock (V1)."
    );
}

/// Sync result is provably source-as-of-T0 when the source is mutated
/// mid-operation (working tree mutation between commits).
///
/// This is the canonical acceptance test for the snapshot-reads change.
///
/// Setup:
///   - source has lib pinned at sha_t0 in the COMMITTED lock
///   - source's WORKING TREE lock is overwritten with sha_t1 before sync
///   - sync is started; the pin at T0 should capture sha_t0
///
/// The test confirms:
///   1. Sync succeeds
///   2. Destination's lib is at sha_t0 (the T0 state)
///   3. Destination's post-sync lock pins sha_t0, not sha_t1
///
/// Without snapshot reads (old behaviour), sync would read sha_t1 from
/// the working tree and converge there, making (2) and (3) fail.
#[test]
fn sync_result_is_source_as_of_t0_not_working_tree_mutation() {
    let tmp = common::tempdir().unwrap();
    let tmp = tmp.path();

    // ---- lib repo: two commits ----
    let lib_path = tmp.join("lib2");
    let sha_t0 = init_repo(&lib_path);
    let sha_t1 = make_commit(&lib_path, "post_t0.txt", "after T0\n", "feat: post-T0");

    let lib_url = format!("file://{}", lib_path.display());

    // ---- source workspace ----
    let source_ws = tmp.join("source2");
    let source_proj = source_ws.join("projects").join("myproject");
    std::fs::create_dir_all(&source_proj).unwrap();
    init_repo(&source_proj);

    write_manifest(&source_proj, &[("lib", &lib_url)]);
    std::fs::write(
        source_proj.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    git(&["add", "rwv.yaml", ".gitattributes"], &source_proj);
    git(&["commit", "-m", "chore: manifest + attrs"], &source_proj);

    // Materialise lib in source at sha_t0.
    let source_lib = source_ws.join("lib");
    git(&["clone", lib_url.as_str(), "lib"], &source_ws);
    git(&["checkout", &sha_t0], &source_lib);

    // Commit the lock at T0.
    write_lock(&source_proj, &[("lib", lib_url.as_str(), &sha_t0)]);
    git(&["add", "rwv.lock"], &source_proj);
    git(&["commit", "-m", "lock: T0 snapshot"], &source_proj);

    // Write .rwv-active for source workspace.
    std::fs::write(source_ws.join(".rwv-active"), "myproject\n").unwrap();

    // ------------------------------------------------------------------
    // Mutate: overwrite the source's working-tree lock with T1 SHA but
    // do NOT commit. This is the "mutation after T0" scenario.
    // ------------------------------------------------------------------
    write_lock(&source_proj, &[("lib", lib_url.as_str(), &sha_t1)]);

    // ---- destination workspace (clone of source) ----
    let dest_ws = tmp.join("dest2");
    let dest_projects = dest_ws.join("projects");
    std::fs::create_dir_all(&dest_projects).unwrap();
    git(
        &["clone", source_proj.to_str().unwrap(), "myproject"],
        &dest_projects,
    );
    let dest_proj = dest_projects.join("myproject");

    // Materialise lib in dest at sha_t0.
    let dest_lib = dest_ws.join("lib");
    git(&["clone", lib_url.as_str(), "lib"], &dest_ws);
    git(&["checkout", &sha_t0], &dest_lib);

    // Write .rwv-active for dest workspace.
    std::fs::write(dest_ws.join(".rwv-active"), "myproject\n").unwrap();

    // ---- Run sync ----
    rwv()
        .current_dir(&dest_proj)
        .args(["sync", source_proj.to_str().unwrap(), "--strategy=ff"])
        .assert()
        .success();

    // ---- Assertion 1: lib is at T0 (not T1) ----
    let dest_lib_head = git_out(&["rev-parse", "HEAD"], &dest_lib);
    assert_eq!(
        dest_lib_head, sha_t0,
        "dest lib should be at T0 sha ({sha_t0}), got {dest_lib_head}.\n\
         The working-tree mutation to T1 must not affect the sync result."
    );

    // ---- Assertion 2: the post-sync lock (in dest project) pins T0, not T1 ----
    let dest_lock_content =
        std::fs::read_to_string(dest_proj.join("rwv.lock")).expect("dest lock should exist");
    assert!(
        dest_lock_content.contains(&sha_t0),
        "dest lock should contain T0 sha ({sha_t0}).\n\
         Lock content:\n{dest_lock_content}"
    );
    assert!(
        !dest_lock_content.contains(&sha_t1),
        "dest lock must NOT contain T1 sha ({sha_t1}) — that was a working-tree-only mutation.\n\
         Lock content:\n{dest_lock_content}"
    );
}

/// `read_file_at_revision` returns the file content at a specific commit.
///
/// This directly tests the new `Vcs::read_file_at_revision` method on
/// the git backend, verifying that:
///   - It returns the content at the specified revision
///   - Subsequent working-tree modifications do not affect the read
#[test]
fn read_file_at_revision_returns_committed_content() {
    let tmp = common::tempdir().unwrap();
    let tmp = tmp.path();

    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&["init", "-b", "main"], &repo);

    // Commit v1.
    std::fs::write(repo.join("file.txt"), "version one\n").unwrap();
    git(&["add", "file.txt"], &repo);
    git(&["commit", "-m", "v1"], &repo);
    let sha_v1 = git_out(&["rev-parse", "HEAD"], &repo);
    let rev_v1 = repoweave::vcs::ResolvedRevisionId::from_canonical(sha_v1.clone(), None);

    // Commit v2.
    std::fs::write(repo.join("file.txt"), "version two\n").unwrap();
    git(&["add", "file.txt"], &repo);
    git(&["commit", "-m", "v2"], &repo);
    let sha_v2 = git_out(&["rev-parse", "HEAD"], &repo);
    let rev_v2 = repoweave::vcs::ResolvedRevisionId::from_canonical(sha_v2, None);

    // Overwrite the working tree with "version three" (uncommitted).
    std::fs::write(repo.join("file.txt"), "version three\n").unwrap();

    let vcs = repoweave::git::git_vcs();

    // Reading at rev_v1 should give "version one" regardless of WT state.
    let content_v1 = vcs
        .read_file_at_revision(&repo, &rev_v1, std::path::Path::new("file.txt"))
        .expect("read at v1 should succeed");
    assert!(
        content_v1.contains("version one"),
        "Expected 'version one', got: {content_v1}"
    );

    // Reading at rev_v2 should give "version two" regardless of WT state.
    let content_v2 = vcs
        .read_file_at_revision(&repo, &rev_v2, std::path::Path::new("file.txt"))
        .expect("read at v2 should succeed");
    assert!(
        content_v2.contains("version two"),
        "Expected 'version two', got: {content_v2}"
    );

    // The working-tree file is "version three" — the revision reads must
    // have been immune to that mutation.
    let wt_content =
        std::fs::read_to_string(repo.join("file.txt")).expect("working tree file should exist");
    assert!(
        wt_content.contains("version three"),
        "Working tree should still have 'version three'"
    );
}
