//! Integration tests anchoring which verbs run intent-mode activation
//! (author managed/generated content) versus context-mode activation
//! (surface + verify only, never author). Source anchor: `ActivationMode`
//! in `src/activate.rs`.
//!
//! Doc claims pinned here, checked behaviourally against a content-derived
//! generated file rather than by grepping comments:
//!   - `rwv add` / `rwv remove` author (intent mode)
//!   - `rwv fetch`, `rwv activate`, workweave-create, `rwv init`, and
//!     `rwv init --adopt` do not author (context mode)
//!   - `rwv init --adopt` additionally must not clobber content the
//!     adopted repo already committed: it clones only the one repo, so
//!     re-authoring there would recompute owned content from a tree
//!     missing every other manifest member
//!
//! `rwv update` authoring and `rwv lock`'s exemption are pinned elsewhere
//! (`verb_shape_test.rs`, plus `lock_test.rs` / `doc_claims_lock_test.rs`
//! for lock) — not duplicated here.
//!
//! Self-contained per the `doc_claims_*` file convention.

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process;

mod common;

fn rwv() -> Command {
    common::rwv()
}

fn git_run(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git command failed to start");
    assert!(
        status.success(),
        "git {:?} failed in {}",
        args,
        dir.display()
    );
}

fn init_bare_repo(path: &Path) {
    let status = common::git()
        .args(["init", "--bare", "--initial-branch=main"])
        .arg(path)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git init --bare failed");
    assert!(status.success());
}

/// A bare repo with one commit, so it can be cloned/fetched from.
fn init_bare_repo_with_commit(path: &Path) {
    init_bare_repo(path);
    let tmp = tempfile::tempdir().expect("tempdir for working clone");
    let work = tmp.path().join("work");
    git_run(
        &["clone", &path.to_string_lossy(), &work.to_string_lossy()],
        tmp.path(),
    );
    std::fs::write(work.join("README"), "init\n").unwrap();
    git_run(&["add", "."], &work);
    git_run(&["commit", "-m", "initial"], &work);
    git_run(&["push", "origin", "main"], &work);
}

/// A bare "project" repo whose HEAD carries `rwv.yaml` (only) declaring
/// `repos`. Used as an `rwv fetch` / `rwv init --adopt` source.
fn make_project_bare(tmp: &Path, name: &str, repos: &[(&str, &str)]) -> PathBuf {
    let bare = tmp.join(format!("{name}.git"));
    init_bare_repo(&bare);
    let work = tmp.join(format!("{name}-work"));
    git_run(
        &["clone", &bare.to_string_lossy(), &work.to_string_lossy()],
        tmp,
    );
    write_manifest(&work, repos);
    git_run(&["add", "."], &work);
    git_run(&["commit", "-m", "manifest"], &work);
    git_run(&["push", "origin", "main"], &work);
    bare
}

/// A minimal workspace: `github/` registry marker + empty `projects/`.
fn make_workspace(tmp: &Path) -> PathBuf {
    let ws = tmp.join("ws");
    std::fs::create_dir_all(ws.join("github")).unwrap();
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    ws
}

/// A workspace with one project whose directory is itself a git repo with
/// `rwv.yaml` committed, and `.rwv-active` pointing at it. This is the shape
/// `find_project_dir` requires for action verbs (`add`/`remove`).
fn setup_git_project(tmp: &Path, project: &str, repos: &[(&str, &str)]) -> (PathBuf, PathBuf) {
    let ws = make_workspace(tmp);
    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();

    git_run(&["init", "--initial-branch=main"], &project_dir);
    write_manifest(&project_dir, repos);
    git_run(&["add", "."], &project_dir);
    git_run(&["commit", "-m", "init"], &project_dir);

    std::fs::write(ws.join(".rwv-active"), format!("{project}\n")).unwrap();
    (ws, project_dir)
}

fn write_manifest(dir: &Path, repos: &[(&str, &str)]) {
    let mut yaml = String::from("repositories:\n");
    if repos.is_empty() {
        yaml.push_str("  {}\n");
    }
    for (path, url) in repos {
        yaml.push_str(&format!(
            "  {path}:\n    type: git\n    url: {url}\n    version: main\n    role: owned\n"
        ));
    }
    std::fs::write(dir.join("rwv.yaml"), yaml).unwrap();
}

/// The `vscode-workspace` integration's file: default-enabled and declared
/// unconditionally for the active project, so its presence after a verb
/// runs is proof that intent-mode activation ran.
fn code_workspace(project_dir: &Path, project: &str) -> PathBuf {
    project_dir.join(format!("{project}.code-workspace"))
}

// ===========================================================================
// Intent verbs: add / remove author managed content
// ===========================================================================

#[test]
fn add_authors_managed_content() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, project_dir) = setup_git_project(tmp.path(), "myapp", &[]);

    assert!(
        !code_workspace(&project_dir, "myapp").exists(),
        "nothing has activated yet"
    );

    let bare = tmp.path().join("dep.git");
    init_bare_repo_with_commit(&bare);
    let url = format!("file://{}", bare.display());

    rwv()
        .args(["add", &url])
        .current_dir(&ws)
        .assert()
        .success();

    assert!(
        code_workspace(&project_dir, "myapp").exists(),
        "`rwv add` is an intent verb; it must author the vscode-workspace file"
    );
}

#[test]
fn remove_authors_managed_content() {
    let tmp = tempfile::tempdir().unwrap();
    let bare = tmp.path().join("dep.git");
    init_bare_repo_with_commit(&bare);
    let url = format!("file://{}", bare.display());

    let (ws, project_dir) = setup_git_project(tmp.path(), "myapp", &[("local/org/dep", &url)]);

    let repo_dir = ws.join("local/org/dep");
    std::fs::create_dir_all(repo_dir.parent().unwrap()).unwrap();
    git_run(
        &[
            "clone",
            &bare.to_string_lossy(),
            &repo_dir.to_string_lossy(),
        ],
        tmp.path(),
    );

    assert!(
        !code_workspace(&project_dir, "myapp").exists(),
        "nothing has activated yet"
    );

    rwv()
        .args(["remove", "local/org/dep"])
        .current_dir(&ws)
        .assert()
        .success();

    assert!(
        code_workspace(&project_dir, "myapp").exists(),
        "`rwv remove` is an intent verb; it must author the vscode-workspace file"
    );
}

// ===========================================================================
// Context verbs: fetch / activate / workweave-create / init do not author
// ===========================================================================

#[test]
fn fetch_does_not_author_managed_content() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    let project_bare = make_project_bare(tmp.path(), "myapp", &[]);
    let source = format!("file://{}", project_bare.display());

    rwv()
        .args(["fetch", &source])
        .current_dir(&ws)
        .assert()
        .success();

    let project_dir = ws.join("projects/myapp");
    assert!(
        project_dir.join("rwv.yaml").exists(),
        "fetch should clone the project"
    );
    assert!(
        !code_workspace(&project_dir, "myapp").exists(),
        "`rwv fetch` is a context verb; it must not author managed content"
    );
}

#[test]
fn activate_does_not_author_managed_content() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let project_dir = ws.join("projects/myapp");
    std::fs::create_dir_all(&project_dir).unwrap();
    write_manifest(&project_dir, &[]);

    assert!(!code_workspace(&project_dir, "myapp").exists());

    rwv()
        .args(["activate", "myapp", "--no-install"])
        .current_dir(&ws)
        .assert()
        .success();

    assert!(
        !code_workspace(&project_dir, "myapp").exists(),
        "`rwv activate` is a context verb; it must not author managed content"
    );
}

#[test]
fn workweave_create_does_not_author_managed_content() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let project_dir = ws.join("projects/myapp");
    std::fs::create_dir_all(&project_dir).unwrap();
    write_manifest(&project_dir, &[]);

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "myapp", "create", "feat"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_project_dir = weaveroot.join("myapp--feat/projects/myapp");
    assert!(
        !code_workspace(&ww_project_dir, "myapp").exists(),
        "workweave-create is a context verb; it must not author managed content"
    );
}

#[test]
fn init_does_not_author_managed_content() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    rwv()
        .args(["init", "myapp"])
        .current_dir(&ws)
        .assert()
        .success();

    let project_dir = ws.join("projects/myapp");
    assert!(project_dir.join("rwv.yaml").exists());
    assert!(
        !code_workspace(&project_dir, "myapp").exists(),
        "`rwv init` is a context verb; it must not author managed content"
    );
}

#[test]
fn init_adopt_does_not_author_managed_content() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());

    let project_bare = make_project_bare(tmp.path(), "adoptee", &[]);
    let source = format!("file://{}", project_bare.display());

    rwv()
        .args(["init", "--adopt", &source])
        .current_dir(&ws)
        .assert()
        .success();

    let project_dir = ws.join("projects/adoptee");
    assert!(project_dir.join("rwv.yaml").exists());
    assert!(
        !code_workspace(&project_dir, "adoptee").exists(),
        "`rwv init --adopt` is a context verb; it must not author managed content"
    );
}

// ===========================================================================
// `rwv init --adopt` must not clobber content the adopted repo already
// committed
//
// `init --adopt` clones only the adopted project's own repo — none of the
// manifest's other members land on disk. If it ever ran intent-mode
// activation instead of context mode, it would open and re-author every
// managed file from that partial tree, clobbering whatever was committed.
// vscode-workspace has no early-return: unlike cargo-workspace (which
// no-ops when it detects no active Rust repos on disk — not the case here,
// since the dep repo never gets cloned), it always parses-and-rewrites the
// `.code-workspace` file when the intent-mode `activate()` path runs, and
// never touches it under context-mode `verify()`. A hand-written, compact
// (non-pretty-printed) `.code-workspace` file makes the two paths provably
// distinguishable: `activate()` re-serializes via `to_string_pretty`, so
// any re-authoring reformats it even if the semantic content ends up the
// same; a bare surface-and-verify leaves the bytes untouched.
// ===========================================================================

#[test]
fn init_adopt_does_not_clobber_committed_generated_content() {
    let tmp = tempfile::tempdir().unwrap();
    let bare = tmp.path().join("myapp.git");
    init_bare_repo(&bare);
    let work = tmp.path().join("myapp-work");
    git_run(
        &["clone", &bare.to_string_lossy(), &work.to_string_lossy()],
        tmp.path(),
    );

    write_manifest(&work, &[]);
    let committed_code_workspace = "{\"folders\":[{\"path\":\".\"}]}";
    std::fs::write(work.join("myapp.code-workspace"), committed_code_workspace).unwrap();
    git_run(&["add", "."], &work);
    git_run(
        &["commit", "-m", "rwv.yaml + committed code-workspace"],
        &work,
    );
    git_run(&["push", "origin", "main"], &work);

    let adopt_ws = make_workspace(tmp.path());
    let source = format!("file://{}", bare.display());

    rwv()
        .args(["init", "--adopt", &source])
        .current_dir(&adopt_ws)
        .assert()
        .success();

    let adopted = std::fs::read_to_string(adopt_ws.join("projects/myapp/myapp.code-workspace"))
        .expect("adopted project should still carry the committed .code-workspace file");
    assert_eq!(
        adopted, committed_code_workspace,
        "`rwv init --adopt` must not re-author the adopted repo's committed \
         .code-workspace file; it clones only the project repo, so re-authoring \
         here would clobber committed content with a recompute from a partial tree"
    );
}
