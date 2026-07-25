//! Integration tests anchoring which verbs run intent-mode activation
//! (author managed/generated content) versus context-mode activation
//! (surface + verify only, never author). Source anchor: `ActivationMode`
//! in `src/activate.rs`.
//!
//! Doc claims pinned here, checked behaviourally against a content-derived
//! generated file rather than by grepping comments:
//!   - `rwv add` / `rwv remove` author (intent mode)
//!   - `rwv add` / `rwv remove` author only when every active repo the
//!     manifest declares is on disk; over a partial member set they record
//!     the membership change and leave the managed files alone
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

/// A bare repo whose single commit carries a `go.mod`, so a clone of it is
/// detected as a `go-work` member and appears in the generated `use` block.
fn init_bare_go_module(path: &Path, module: &str) {
    init_bare_repo(path);
    let tmp = tempfile::tempdir().expect("tempdir for working clone");
    let work = tmp.path().join("work");
    git_run(
        &["clone", &path.to_string_lossy(), &work.to_string_lossy()],
        tmp.path(),
    );
    std::fs::write(work.join("go.mod"), format!("module {module}\n\ngo 1.21\n")).unwrap();
    git_run(&["add", "."], &work);
    git_run(&["commit", "-m", "initial"], &work);
    git_run(&["push", "origin", "main"], &work);
}

/// Clone `bare` to `repo_path` under the workspace, the way `rwv fetch` would.
fn clone_member(bare: &Path, ws: &Path, repo_path: &str) {
    let dest = ws.join(repo_path);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    git_run(
        &["clone", &bare.to_string_lossy(), &dest.to_string_lossy()],
        ws,
    );
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

/// The `go-work` integration's file. Unlike the vscode workspace it lists the
/// members themselves, so it shows *which* repos an authoring pass saw.
fn go_work(project_dir: &Path) -> PathBuf {
    project_dir.join("go.work")
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
// Intent verbs author only over a whole member set
// ===========================================================================

/// A repo declared in `rwv.yaml` but not cloned is not *pending* for the
/// integrations, it is invisible — so `rwv add` over a partially-fetched
/// workspace must record the membership change without authoring.
#[test]
fn add_does_not_author_from_a_partial_member_set() {
    let tmp = tempfile::tempdir().unwrap();

    let absent_bare = tmp.path().join("org/absent.git");
    init_bare_go_module(&absent_bare, "example.com/absent");
    let absent_url = format!("file://{}", absent_bare.display());

    // `org/absent` is declared and never cloned: the partially-fetched shape
    // an `rwv init --adopt` (or a failed clone) leaves behind.
    let (ws, project_dir) = setup_git_project(tmp.path(), "myapp", &[("org/absent", &absent_url)]);

    let new_bare = tmp.path().join("org/newdep.git");
    init_bare_go_module(&new_bare, "example.com/newdep");
    let new_url = format!("file://{}", new_bare.display());

    rwv()
        .args(["add", &new_url])
        .current_dir(&ws)
        .assert()
        .success();

    let manifest = std::fs::read_to_string(project_dir.join("rwv.yaml")).unwrap();
    assert!(
        manifest.contains("org/newdep"),
        "`rwv add` must still record the membership change, got:\n{manifest}"
    );
    assert!(
        !code_workspace(&project_dir, "myapp").exists(),
        "`rwv add` must not author while a declared repo is missing from disk"
    );
    assert!(
        !go_work(&project_dir).exists(),
        "`rwv add` must not author a member list that omits the missing repo"
    );
}

/// The sharp end: an already-committed managed file listing every member,
/// and a `rwv remove` run after one of the others left the disk. Authoring
/// there would rewrite the file from what remains and drop the rest.
#[test]
fn remove_does_not_overwrite_managed_content_from_a_partial_member_set() {
    let tmp = tempfile::tempdir().unwrap();

    let keep_bare = tmp.path().join("org/keep.git");
    init_bare_go_module(&keep_bare, "example.com/keep");
    let gone_bare = tmp.path().join("org/gone.git");
    init_bare_go_module(&gone_bare, "example.com/gone");
    let keep_url = format!("file://{}", keep_bare.display());
    let gone_url = format!("file://{}", gone_bare.display());

    let (ws, project_dir) = setup_git_project(
        tmp.path(),
        "myapp",
        &[("org/keep", &keep_url), ("org/gone", &gone_url)],
    );
    clone_member(&keep_bare, &ws, "org/keep");
    clone_member(&gone_bare, &ws, "org/gone");

    // Author once over the whole member set, via an intent verb.
    let drop_bare = tmp.path().join("org/drop.git");
    init_bare_go_module(&drop_bare, "example.com/drop");
    let drop_url = format!("file://{}", drop_bare.display());
    rwv()
        .args(["add", &drop_url])
        .current_dir(&ws)
        .assert()
        .success();

    let authored = std::fs::read_to_string(go_work(&project_dir))
        .expect("`rwv add` over a whole member set authors go.work");
    for member in ["org/keep", "org/gone", "org/drop"] {
        assert!(
            authored.contains(member),
            "go.work should list {member}, got:\n{authored}"
        );
    }

    std::fs::remove_dir_all(ws.join("org/gone")).unwrap();

    rwv()
        .args(["remove", "org/drop"])
        .current_dir(&ws)
        .assert()
        .success();

    let manifest = std::fs::read_to_string(project_dir.join("rwv.yaml")).unwrap();
    assert!(
        !manifest.contains("org/drop"),
        "`rwv remove` must still record the membership change, got:\n{manifest}"
    );
    assert_eq!(
        std::fs::read_to_string(go_work(&project_dir)).unwrap(),
        authored,
        "`rwv remove` must leave go.work alone while a declared repo is missing from disk"
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
