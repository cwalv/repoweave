//! Integration tests anchoring documented behavior of `rwv update`.
//!
//! Doc claims pinned here:
//!
//!   - update re-snapshots `rwv.lock` from each manifest repo's branch HEAD
//!     (not from the previous lock SHA)
//!   - update is the network-bumping counterpart to `rwv fetch` (which is
//!     lock-aligning only); the two verbs are distinct
//!   - update advances disk state to the freshly-fetched branch HEAD before
//!     re-snapshotting the lock
//!   - update -j N runs the per-repo advance loop in parallel and still
//!     writes a single coherent lock at the end
//!   - update --json emits an envelope { "$schema": ..., "repos": [...] }
//!   - update --json -j N (N > 1) streams NDJSON, one record per repo
//!   - update --commit lands the regenerated integration content in the
//!     same commit as the lock bump that caused it, and nothing else:
//!     unrelated work in progress still refuses, a filtered run (which
//!     authors nothing) commits the lock alone, and a generated file the
//!     operator gitignored stays out
//!
//! Style note: this fixture is the bare-remote-plus-clone-plus-project
//! shape from `update_test.rs`; we keep it local rather than forking
//! helpers (the constraint from the spec) since `update_test.rs` doesn't
//! expose its helpers and the doc_claims_* convention is to be
//! self-contained per file. The verb-vs-fetch contrast claim is the
//! reason this file exists at all.

use assert_cmd::Command;
use repoweave::update::{UPDATE_RECORD_SCHEMA_URL, UPDATE_SCHEMA_URL};
use serde_json::Value;
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

/// Push a new commit on `main` to a bare repo via a working clone. Returns
/// the new HEAD SHA on the bare. The bare's name is embedded in the commit
/// content to keep distinct bares' SHAs distinct.
fn advance_bare_main(bare: &Path) -> String {
    let parent = bare.parent().unwrap();
    let stem = bare.file_stem().unwrap().to_string_lossy().into_owned();
    let work = parent.join(format!("__adv_{stem}"));
    git_run(
        parent,
        &["clone", bare.to_str().unwrap(), work.to_str().unwrap()],
    );
    git_run(&work, &["config", "user.email", "test@test.com"]);
    git_run(&work, &["config", "user.name", "Test"]);
    std::fs::write(work.join("advance.txt"), format!("advance-{stem}")).unwrap();
    git_run(&work, &["add", "."]);
    git_run(&work, &["commit", "-m", &format!("advance {stem}")]);
    git_run(&work, &["push", "origin", "main"]);
    git_run(&work, &["rev-parse", "HEAD"])
}

struct UpdateWorkspace {
    _tmp: tempfile::TempDir,
    workspace: PathBuf,
    project_name: String,
    manifest_bares: Vec<(String, PathBuf)>,
}

/// Build a workspace with manifest repos at the given roles. The lock
/// initially matches local HEAD (which itself matches the bare's HEAD).
fn build_workspace(project_name: &str, repos: &[(&str, &str)]) -> UpdateWorkspace {
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
        let bare_url = common::file_url(&bare);
        manifest_yaml.push_str(&format!(
            "[repositories.\"{repo_path}\"]\ntype = \"git\"\nurl = \"{bare_url}\"\nversion = \"main\"\nrole = \"{role}\"\n"
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

    std::fs::write(project_dir.join("rwv.toml"), &manifest_yaml).unwrap();
    // Round-trips through the real parser + `lock::write_lock`: a
    // hand-formatted string that differs only in whitespace from what
    // `rwv lock` itself would emit still diffs against a real relock.
    let mut lock_entries = Vec::new();
    for (rp, sha) in &manifest_shas {
        let (_, bare) = manifest_bares.iter().find(|(p, _)| p == rp).unwrap();
        let bare_url = common::file_url(bare);
        lock_entries.push(format!(
            "{rp:?}: {{\"type\": \"git\", \"url\": {bare_url:?}, \"version\": {sha:?}}}"
        ));
    }
    let raw_lock = format!("{{\"repositories\": {{{}}}}}", lock_entries.join(","));
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
    git_run(&project_dir, &["add", "."]);
    git_run(&project_dir, &["commit", "-m", "manifest + lock"]);

    std::fs::write(workspace.join(".rwv-active"), format!("{project_name}\n")).unwrap();

    UpdateWorkspace {
        _tmp: tmp,
        workspace,
        project_name: project_name.to_string(),
        manifest_bares,
    }
}

fn read_lock_sha(workspace: &Path, project_name: &str, repo_path: &str) -> String {
    let lock_path = workspace
        .join("projects")
        .join(project_name)
        .join("rwv.lock");
    let lock = repoweave::manifest::LockFile::from_path(&lock_path)
        .expect("rwv.lock should exist and parse after update");
    let path = repoweave::manifest::RepoPath::new(repo_path).expect("known-safe literal");
    lock.get_entry(&path)
        .unwrap_or_else(|| panic!("could not find version for {repo_path} in lock:\n{lock:?}"))
        .version
        .as_str()
        .to_string()
}

// ===========================================================================
// 1. update re-snapshots the lock from branch HEAD
//
// Doc claim: after `rwv update`, the rwv.lock entry for each updated repo
// equals the new branch-HEAD SHA on the remote (not the prior lock SHA).
// ===========================================================================

#[test]
fn update_re_snapshots_lock_from_branch_head() {
    let repos = [("local/org/a", "owned")];
    let ws = build_workspace("alpha", &repos);

    let initial_lock_sha = read_lock_sha(&ws.workspace, &ws.project_name, "local/org/a");

    // Advance the remote so HEAD moves past the initial lock.
    let (_, bare) = &ws.manifest_bares[0];
    let new_remote_head = advance_bare_main(bare);
    assert_ne!(initial_lock_sha, new_remote_head);

    rwv()
        .args(["update", "--dirty"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    // Lock now reflects the new branch HEAD.
    let post_lock_sha = read_lock_sha(&ws.workspace, &ws.project_name, "local/org/a");
    assert_eq!(
        post_lock_sha, new_remote_head,
        "update should re-snapshot the lock from the freshly-fetched branch HEAD"
    );

    // Local clone HEAD also matches the new SHA.
    let local_head = git_run(&ws.workspace.join("local/org/a"), &["rev-parse", "HEAD"]);
    assert_eq!(
        local_head, new_remote_head,
        "update should advance the local clone to the new branch HEAD"
    );
}

// ===========================================================================
// 2. update is distinct from fetch (verb-vocabulary split)
//
// Doc claim: `rwv fetch` (default mode) aligns the clone to the existing
// rwv.lock and does not advance to remote HEAD. `rwv update` advances to
// remote HEAD and re-snapshots the lock. These are different verbs with
// different side-effects.
//
// We exercise the distinction directly: starting from the same state, run
// `rwv fetch` and `rwv update` in two separate workspaces and observe that
// only the update path moves the lock SHA.
// ===========================================================================

#[test]
fn update_advances_lock_while_fetch_does_not() {
    // Build two workspaces from the same bare-repo seed. The bare advances
    // past the initial lock SHA in both, then:
    //   - ws_fetch:  run `rwv fetch` from inside the active project dir
    //                (default mode = aligns to lock, does not bump it).
    //   - ws_update: run `rwv update` (bumps lock to branch HEAD).
    //
    // The lock SHA in ws_fetch must be unchanged; the lock SHA in
    // ws_update must equal the new branch HEAD.
    let repos = [("local/org/a", "owned")];

    // --- ws_update --------------------------------------------------------
    let ws_update = build_workspace("alpha", &repos);
    let initial_lock_sha_update =
        read_lock_sha(&ws_update.workspace, &ws_update.project_name, "local/org/a");
    let (_, bare_update) = &ws_update.manifest_bares[0];
    let new_head_update = advance_bare_main(bare_update);
    assert_ne!(initial_lock_sha_update, new_head_update);

    rwv()
        .args(["update", "--dirty"])
        .current_dir(&ws_update.workspace)
        .assert()
        .success();

    let post_lock_update =
        read_lock_sha(&ws_update.workspace, &ws_update.project_name, "local/org/a");
    assert_eq!(
        post_lock_update, new_head_update,
        "rwv update must move the lock to the new branch HEAD"
    );

    // --- ws_fetch ---------------------------------------------------------
    // Default `rwv fetch <source>` is a clone-and-align verb that creates a
    // fresh workspace from a project source. It is NOT the same as
    // `rwv update` (a re-snapshot verb on an existing workspace). Verify
    // that aspect of the split: `rwv fetch <project_source>` reads the
    // committed lock and pins clones at the lock SHA — the bare's new
    // HEAD does NOT leak into the clone.
    //
    // We mirror fetch_test.rs's setup: a bare project repo carrying rwv.toml
    // + rwv.lock, fetched into an empty workspace. The fetched clone of
    // the manifest repo must be at the LOCK sha, not the new bare HEAD.
    let tmp_fetch = common::tempdir().unwrap();
    let ws_fetch = tmp_fetch.path().join("ws");
    std::fs::create_dir_all(&ws_fetch).unwrap();

    // Set up the manifest-repo bare and capture the initial SHA.
    let manifest_bare = tmp_fetch.path().join("manifest.git");
    init_bare_repo_with_commit(&manifest_bare);
    let manifest_url = common::file_url(&manifest_bare);
    let dep_clone = tmp_fetch.path().join("dep_clone");
    git_run(
        tmp_fetch.path(),
        &[
            "clone",
            manifest_bare.to_str().unwrap(),
            dep_clone.to_str().unwrap(),
        ],
    );
    let initial_sha = git_run(&dep_clone, &["rev-parse", "HEAD"]);

    // Build the project bare with rwv.toml + rwv.lock pinning to initial_sha.
    let project_bare = tmp_fetch.path().join("project.git");
    init_bare_repo_with_commit(&project_bare);
    let project_work = tmp_fetch.path().join("project_work");
    git_run(
        tmp_fetch.path(),
        &[
            "clone",
            project_bare.to_str().unwrap(),
            project_work.to_str().unwrap(),
        ],
    );
    git_run(&project_work, &["config", "user.email", "test@test.com"]);
    git_run(&project_work, &["config", "user.name", "Test"]);
    let manifest_toml = format!(
        "[repositories.\"local/team/dep\"]\ntype = \"git\"\nurl = \"{manifest_url}\"\nversion = \"main\"\nrole = \"owned\"\n"
    );
    std::fs::write(project_work.join("rwv.toml"), manifest_toml).unwrap();
    let raw_lock = format!(
        "{{\"repositories\": {{\"local/team/dep\": {{\"type\": \"git\", \"url\": {manifest_url:?}, \"version\": {initial_sha:?}}}}}}}"
    );
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    repoweave::lock::write_lock(&lock, &project_work.join("rwv.lock")).unwrap();
    git_run(&project_work, &["add", "."]);
    git_run(&project_work, &["commit", "-m", "manifest + lock"]);
    git_run(&project_work, &["push", "origin", "main"]);

    // Advance the bare past the lock — proves fetch reads lock, not HEAD.
    git_run(&dep_clone, &["config", "user.email", "test@test.com"]);
    git_run(&dep_clone, &["config", "user.name", "Test"]);
    std::fs::write(dep_clone.join("after.txt"), "after").unwrap();
    git_run(&dep_clone, &["add", "."]);
    git_run(&dep_clone, &["commit", "-m", "after-lock"]);
    git_run(&dep_clone, &["push", "origin", "main"]);

    let project_source = common::file_url(&project_bare);
    rwv()
        .args(["fetch", &project_source])
        .current_dir(&ws_fetch)
        .assert()
        .success();

    // The cloned dep should sit at initial_sha — proves default `rwv fetch`
    // does not bump.
    let fetched_head = git_run(&ws_fetch.join("local/team/dep"), &["rev-parse", "HEAD"]);
    assert_eq!(
        fetched_head, initial_sha,
        "rwv fetch (default) aligns to the lock; it must NOT advance to branch HEAD"
    );
}

// ===========================================================================
// 3. update -j N parallel mode
//
// Doc claim: `rwv update -j N` (N > 1) advances each manifest repo on a
// bounded worker pool; the lock write happens serially after the pool
// joins. The per-repo lines carry the `[<repo>]` prefix and every repo
// ends at its new branch HEAD.
// ===========================================================================

#[test]
fn update_dash_j_parallel_advances_all_and_emits_prefix() {
    let repos = [
        ("local/org/a", "owned"),
        ("local/org/b", "owned"),
        ("local/org/c", "owned"),
    ];
    let ws = build_workspace("alpha", &repos);

    // Advance each bare to a new HEAD.
    let mut new_heads: Vec<(String, String)> = Vec::new();
    for (rp, bare) in &ws.manifest_bares {
        let new = advance_bare_main(bare);
        new_heads.push((rp.clone(), new));
    }

    let output = rwv()
        .args(["update", "--dirty", "-j", "2"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv update -j 2");
    assert!(
        output.status.success(),
        "update -j 2 should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Reporter::Parallel wraps each repo's lines with `[<repo>]`. The exact
    // text varies (git output is captured) but at minimum the per-repo
    // "rwv update: fetching <path>" line is emitted under the prefix.
    let any_prefix = new_heads
        .iter()
        .any(|(rp, _)| stdout.contains(&format!("[{rp}]")));
    assert!(
        any_prefix,
        "update -j N must wrap per-repo lines with `[<repo>]`; got:\n{stdout}"
    );

    // Every local clone now sits at its new branch HEAD.
    for (rp, new) in &new_heads {
        let head = git_run(&ws.workspace.join(rp), &["rev-parse", "HEAD"]);
        assert_eq!(&head, new, "{rp} local should be at the new branch HEAD");
    }

    // Lock re-snapshot covered every repo.
    for (rp, new) in &new_heads {
        let lock_sha = read_lock_sha(&ws.workspace, &ws.project_name, rp);
        assert_eq!(
            &lock_sha, new,
            "lock entry for {rp} should reflect the new branch HEAD after update -j"
        );
    }
}

// ===========================================================================
// 4. update --json emits envelope (--json -j 1)
//
// Doc claim: `rwv update --json` (with -j 1) emits a JSON envelope
// `{ "$schema": "<url>", "repos": [...] }`. Per-repo records include
// `path`, `absolute_path`, `branch`, `kind`, `old_sha`, `new_sha`.
// ===========================================================================

#[test]
fn update_json_emits_envelope_under_j1() {
    let repos = [("local/org/a", "owned")];
    let ws = build_workspace("alpha", &repos);

    // Advance the remote so the repo has a new HEAD to pull.
    let (_, bare) = &ws.manifest_bares[0];
    let new_sha = advance_bare_main(bare);

    let assert = rwv()
        .args(["update", "--dirty", "--json", "-j", "1"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout must be JSON envelope ({e}):\n{stdout}"));

    let obj = parsed.as_object().expect("top level must be an object");
    assert_eq!(
        obj.get("$schema").and_then(Value::as_str),
        Some(UPDATE_SCHEMA_URL),
        "envelope must have correct $schema URL"
    );

    let repos_arr = obj
        .get("repos")
        .and_then(Value::as_array)
        .expect("repos must be an array");
    assert_eq!(repos_arr.len(), 1, "one repo in manifest");

    let rec = &repos_arr[0];
    assert_eq!(rec["path"], "local/org/a");
    assert_eq!(rec["branch"], "main");
    assert!(
        rec.get("absolute_path").and_then(Value::as_str).is_some(),
        "absolute_path must be present"
    );
    // kind must be "updated" because the remote advanced past the initial lock.
    assert_eq!(
        rec["kind"], "updated",
        "repo was advanced so kind should be 'updated'"
    );
    assert_eq!(
        rec["new_sha"].as_str(),
        Some(new_sha.as_str()),
        "new_sha must equal the new branch HEAD"
    );
}

// ===========================================================================
// 5. update --json -j N emits NDJSON (N > 1)
//
// Doc claim: `rwv update --json -j N` with N > 1 streams NDJSON. Each
// line is a self-describing JSON record with `$schema` embedded. No
// envelope wrapper is emitted.
// ===========================================================================

#[test]
fn update_json_emits_ndjson_under_j_gt_1() {
    let repos = [("local/org/a", "owned"), ("local/org/b", "owned")];
    let ws = build_workspace("alpha", &repos);

    // Advance each remote.
    for (_, bare) in &ws.manifest_bares {
        advance_bare_main(bare);
    }

    let assert = rwv()
        .args(["update", "--dirty", "--json", "-j", "2"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    // The whole stdout must NOT parse as one JSON document (proves NDJSON).
    assert!(
        serde_json::from_str::<Value>(&stdout).is_err(),
        "NDJSON stdout must not parse as one document:\n{stdout}"
    );

    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(
        lines.len() >= repos.len(),
        "expected >= {} NDJSON lines, got {}:\n{stdout}",
        repos.len(),
        lines.len()
    );

    let mut seen_paths = std::collections::BTreeSet::new();
    for line in &lines {
        let v: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("NDJSON line not valid JSON ({e}): {line}"));
        let obj = v.as_object().unwrap();
        assert_eq!(
            obj.get("$schema").and_then(Value::as_str),
            Some(UPDATE_RECORD_SCHEMA_URL),
            "every NDJSON record must embed $schema: {line}"
        );
        assert!(obj.contains_key("kind"), "missing kind: {line}");
        assert!(obj.contains_key("path"), "missing path: {line}");
        assert!(
            obj.contains_key("absolute_path"),
            "missing absolute_path: {line}"
        );
        if let Some(p) = obj.get("path").and_then(Value::as_str) {
            seen_paths.insert(p.to_string());
        }
    }
    for (rp, _) in &repos {
        assert!(
            seen_paths.contains(*rp),
            "expected {rp} in NDJSON stream; got {seen_paths:?}"
        );
    }
}

// ===========================================================================
// 7. update --commit lands the regenerated content in the lock commit
//
// Doc claim (trigger model): an intent verb authors the new managed region
// "as part of that operation, so the file change lands *in the same commit*,
// alongside the rwv.toml/rwv.lock change that caused it". `update --commit`
// is the only intent verb that makes the commit itself, so it is the only
// one that can break the claim by leaving the derived files behind.
// ===========================================================================

/// Advance `bare`'s main by a commit that adds a `go.mod`, so the member is
/// only detected as a go-work member *after* the update — a generated file
/// that changes because of the advance, not because of membership.
fn advance_bare_main_adding_go_mod(bare: &Path, module: &str) -> String {
    let parent = bare.parent().unwrap();
    let stem = bare.file_stem().unwrap().to_string_lossy().into_owned();
    let work = parent.join(format!("__gomod_{stem}"));
    git_run(
        parent,
        &["clone", bare.to_str().unwrap(), work.to_str().unwrap()],
    );
    git_run(&work, &["config", "user.email", "test@test.com"]);
    git_run(&work, &["config", "user.name", "Test"]);
    std::fs::write(work.join("go.mod"), format!("module {module}\n\ngo 1.21\n")).unwrap();
    git_run(&work, &["add", "."]);
    git_run(&work, &["commit", "-m", "add go.mod"]);
    git_run(&work, &["push", "origin", "main"]);
    git_run(&work, &["rev-parse", "HEAD"])
}

/// Paths touched by the tip commit of the repo at `dir`.
fn files_in_head_commit(dir: &Path) -> Vec<String> {
    git_run(
        dir,
        &["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"],
    )
    .lines()
    .map(str::to_string)
    .collect()
}

#[test]
fn update_commit_lands_generated_files_in_the_lock_commit() {
    let repos = [("local/org/a", "owned")];
    let ws = build_workspace("alpha", &repos);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);

    let (_, bare) = &ws.manifest_bares[0];
    advance_bare_main_adding_go_mod(bare, "example.com/a");

    rwv()
        .args(["update", "--dirty", "--commit"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    let committed = files_in_head_commit(&project_dir);
    assert!(
        committed.contains(&"rwv.lock".to_string()),
        "the lock bump must be in the commit; got {committed:?}"
    );
    assert!(
        committed.contains(&"go.work".to_string()),
        "the generated file the lock bump caused must land in the SAME commit; \
         got {committed:?}"
    );
    assert_eq!(
        git_run(&project_dir, &["status", "--porcelain"]),
        "",
        "nothing the intent verb authored may be left uncommitted"
    );
}

/// A filtered update proves nothing about the repos it skipped, so it
/// withholds authoring — and the commit is then legitimately lock-only.
/// The widened staging set must not force content into that commit.
#[test]
fn update_filtered_commit_is_lock_only() {
    let repos = [("local/org/a", "owned"), ("local/org/b", "owned")];
    let ws = build_workspace("alpha", &repos);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);

    let (_, bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/a")
        .unwrap();
    advance_bare_main_adding_go_mod(bare, "example.com/a");

    rwv()
        .args(["update", "--dirty", "--commit", "--repo=local/org/a"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    assert_eq!(
        files_in_head_commit(&project_dir),
        vec!["rwv.lock".to_string()],
        "a filtered update authors nothing, so its commit carries the lock alone"
    );
}

/// Staging the authored set must not become `commit -a`: work in progress
/// the verb did not produce still blocks the auto-commit.
#[test]
fn update_commit_still_refuses_unrelated_dirt() {
    let repos = [("local/org/a", "owned")];
    let ws = build_workspace("alpha", &repos);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);

    std::fs::write(project_dir.join("notes.md"), "draft\n").unwrap();
    git_run(&project_dir, &["add", "notes.md"]);
    git_run(&project_dir, &["commit", "-m", "notes"]);
    std::fs::write(project_dir.join("notes.md"), "draft, edited\n").unwrap();

    let (_, bare) = &ws.manifest_bares[0];
    advance_bare_main(bare);

    rwv()
        .args(["update", "--dirty", "--commit"])
        .current_dir(&ws.workspace)
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "uncommitted changes outside rwv.lock",
        ));
}

/// A generated file the operator chose to gitignore is not part of the
/// committed triple, so it is not part of the commit either — and its
/// absence must not fail the staging step.
#[test]
fn update_commit_skips_a_gitignored_generated_file() {
    let repos = [("local/org/a", "owned")];
    let ws = build_workspace("alpha", &repos);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);

    std::fs::write(project_dir.join(".gitignore"), "go.work\n").unwrap();
    git_run(&project_dir, &["add", ".gitignore"]);
    git_run(&project_dir, &["commit", "-m", "ignore go.work"]);

    let (_, bare) = &ws.manifest_bares[0];
    advance_bare_main_adding_go_mod(bare, "example.com/a");

    rwv()
        .args(["update", "--dirty", "--commit"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    assert!(
        project_dir.join("go.work").exists(),
        "authoring still writes the file; only the commit skips it"
    );
    assert!(
        !files_in_head_commit(&project_dir).contains(&"go.work".to_string()),
        "an ignored generated file must stay out of the commit"
    );
}
