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
//!   - `rwv init --adopt` additionally must not re-author what the
//!     authoring path owns, even when the adopted repo committed it: it
//!     clones only the one repo, so re-authoring there would recompute
//!     owned content from a tree missing every other manifest member
//!   - the same claim carried by `cargo-workspace`, on both halves of the
//!     merge model: a marked `[workspace].members` is not truncated, and a
//!     user-held (unmarked) `Cargo.toml` gets no `DefaultOnly` key written
//!     into it
//!   - the boundary of that claim: install hooks run in context mode too, so
//!     an adopt DOES regenerate a committed `Cargo.lock`, and DEFERS the
//!     generation entirely when the members are not fetched yet
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
    let tmp = common::tempdir().expect("tempdir for working clone");
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
    let tmp = common::tempdir().expect("tempdir for working clone");
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

/// A bare "project" repo whose HEAD carries `rwv.toml` (only) declaring
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
/// `rwv.toml` committed, and `.rwv-active` pointing at it. This is the shape
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
    let mut manifest_toml = String::from("[repositories]\n");
    for (path, url) in repos {
        manifest_toml.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(dir.join("rwv.toml"), manifest_toml).unwrap();
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
    let tmp = common::tempdir().unwrap();
    let (ws, project_dir) = setup_git_project(tmp.path(), "myapp", &[]);

    assert!(
        !code_workspace(&project_dir, "myapp").exists(),
        "nothing has activated yet"
    );

    let bare = tmp.path().join("dep.git");
    init_bare_repo_with_commit(&bare);
    let url = common::file_url(&bare);

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
    let tmp = common::tempdir().unwrap();
    let bare = tmp.path().join("dep.git");
    init_bare_repo_with_commit(&bare);
    let url = common::file_url(&bare);

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

/// A repo declared in `rwv.toml` but not cloned is not *pending* for the
/// integrations, it is invisible — so `rwv add` over a partially-fetched
/// workspace must record the membership change without authoring.
#[test]
fn add_does_not_author_from_a_partial_member_set() {
    let tmp = common::tempdir().unwrap();

    let absent_bare = tmp.path().join("org/absent.git");
    init_bare_go_module(&absent_bare, "example.com/absent");
    let absent_url = common::file_url(&absent_bare);

    // `org/absent` is declared and never cloned: the partially-fetched shape
    // an `rwv init --adopt` (or a failed clone) leaves behind.
    let (ws, project_dir) = setup_git_project(tmp.path(), "myapp", &[("org/absent", &absent_url)]);

    let new_bare = tmp.path().join("org/newdep.git");
    init_bare_go_module(&new_bare, "example.com/newdep");
    let new_url = common::file_url(&new_bare);

    rwv()
        .args(["add", &new_url])
        .current_dir(&ws)
        .assert()
        .success();

    let manifest = std::fs::read_to_string(project_dir.join("rwv.toml")).unwrap();
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
    let tmp = common::tempdir().unwrap();

    let keep_bare = tmp.path().join("org/keep.git");
    init_bare_go_module(&keep_bare, "example.com/keep");
    let gone_bare = tmp.path().join("org/gone.git");
    init_bare_go_module(&gone_bare, "example.com/gone");
    let keep_url = common::file_url(&keep_bare);
    let gone_url = common::file_url(&gone_bare);

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
    let drop_url = common::file_url(&drop_bare);
    rwv()
        .args(["add", &drop_url])
        .current_dir(&ws)
        .assert()
        .success();

    let authored = std::fs::read_to_string(go_work(&project_dir))
        .expect("`rwv add` over a whole member set authors go.work");
    // `file://` matches no built-in registry, so `drop_url` lands at
    // `local/org/drop`, not the bare `org/drop` its pre-seeded siblings use.
    for member in ["org/keep", "org/gone", "local/org/drop"] {
        assert!(
            authored.contains(member),
            "go.work should list {member}, got:\n{authored}"
        );
    }

    std::fs::remove_dir_all(ws.join("org/gone")).unwrap();

    rwv()
        .args(["remove", "local/org/drop"])
        .current_dir(&ws)
        .assert()
        .success();

    let manifest = std::fs::read_to_string(project_dir.join("rwv.toml")).unwrap();
    assert!(
        !manifest.contains("local/org/drop"),
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
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    let project_bare = make_project_bare(tmp.path(), "myapp", &[]);
    let source = common::file_url(&project_bare);

    rwv()
        .args(["fetch", &source])
        .current_dir(&ws)
        .assert()
        .success();

    let project_dir = ws.join("projects/myapp");
    assert!(
        project_dir.join("rwv.toml").exists(),
        "fetch should clone the project"
    );
    assert!(
        !code_workspace(&project_dir, "myapp").exists(),
        "`rwv fetch` is a context verb; it must not author managed content"
    );
}

#[test]
fn activate_does_not_author_managed_content() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let project_dir = ws.join("projects/myapp");
    std::fs::create_dir_all(&project_dir).unwrap();
    write_manifest(&project_dir, &[]);

    assert!(!code_workspace(&project_dir, "myapp").exists());

    rwv()
        .args(["activate", "myapp", "--no-materialize"])
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
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let project_dir = ws.join("projects/myapp");
    std::fs::create_dir_all(&project_dir).unwrap();
    write_manifest(&project_dir, &[]);

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "myapp", "create", "feat"])
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
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    rwv()
        .args(["init", "myapp"])
        .current_dir(&ws)
        .assert()
        .success();

    let project_dir = ws.join("projects/myapp");
    assert!(project_dir.join("rwv.toml").exists());
    assert!(
        !code_workspace(&project_dir, "myapp").exists(),
        "`rwv init` is a context verb; it must not author managed content"
    );
}

#[test]
fn init_adopt_does_not_author_managed_content() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());

    let project_bare = make_project_bare(tmp.path(), "adoptee", &[]);
    let source = common::file_url(&project_bare);

    rwv()
        .args(["init", "--adopt", &source])
        .current_dir(&ws)
        .assert()
        .success();

    let project_dir = ws.join("projects/adoptee");
    assert!(project_dir.join("rwv.toml").exists());
    assert!(
        !code_workspace(&project_dir, "adoptee").exists(),
        "`rwv init --adopt` is a context verb; it must not author managed content"
    );
}

// ===========================================================================
// `rwv init --adopt` must not re-author what the AUTHORING PATH owns
//
// Scope, per docs/explanation/joints/file-ownership.md §"Install hooks at
// context verbs: lockfiles may be rewritten": "never author" is a claim about
// the authoring path — managed regions and rwv-computed artifacts, which
// intent verbs regenerate and context verbs never run. It is NOT the broader
// claim that an adopt leaves every committed byte alone. Install hooks run in
// context mode too and rewrite the ecosystem lockfiles their tools own, which
// `init_adopt_regenerates_a_committed_cargo_lock` below pins as the other half
// of the same distinction. So this test is named for the artifact it proves it
// on rather than for "committed generated content" as a class: a
// `.code-workspace` and a `Cargo.lock` are both committed generated content
// and their fates differ.
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
fn init_adopt_does_not_reauthor_a_committed_code_workspace() {
    let tmp = common::tempdir().unwrap();
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
        &["commit", "-m", "rwv.toml + committed code-workspace"],
        &work,
    );
    git_run(&["push", "origin", "main"], &work);

    let adopt_ws = make_workspace(tmp.path());
    let source = common::file_url(&bare);

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

// ===========================================================================
// The same no-clobber claim, carried by `cargo-workspace`
//
// The claim above is proven through vscode-workspace because that integration
// writes unconditionally. cargo-workspace is the more consequential case —
// re-authoring `[workspace].members` silently truncates a cargo workspace —
// but it is gated behind `CargoWorkspace::has_active_cargo_work`, which is
// disk-driven: its first arm looks for repos with a root `Cargo.toml` on
// disk, and `init --adopt` clones only the project repo, so that arm is
// always empty at adopt time. A fixture that relies on it produces a no-op in
// BOTH activation modes — a test that passes whether or not the behaviour
// under test is correct.
//
// The gate's second arm is the way in: a repo named by
// `integrations.cargo-workspace.members.<repo>` counts as active cargo work
// from `rwv.toml` alone, no clone required. That is the rvtty shape (a repo
// with no root `Cargo.toml` whose sub-packages are the members), and it is
// what makes the integration live during an adopt.
//
// Two constraints shape the fixtures below.
//
// 1. Every member path named by the *committed* `Cargo.toml` must exist on
//    disk. `init --adopt` runs activate hooks, and cargo-workspace's hook
//    runs `cargo generate-lockfile` against the surfaced manifest; a member
//    that is not on disk makes cargo exit 101 and takes the whole adopt down
//    with it. So the adopting workspace already carries the member repo (an
//    adopt into an existing weave), and the truncation is driven by the
//    config naming fewer members than the committed file lists.
// 2. The committed file must carry the `# managed by rwv` marker for the
//    members axis to bite: `merge_activate` defers an `Author` key when the
//    marker is absent. A committed marked file is the documented shape —
//    operators commit the managed `Cargo.toml` so the composition is
//    reproducible from the repo.
//
// The second test covers the unmarked half, where the members axis is silent
// by construction and the `DefaultOnly` `resolver` key is the discriminator:
// `merge_activate` defers `members` but still injects a missing `resolver`
// and writes the file back.
// ===========================================================================

/// Return early (skip) if `cargo` is not on PATH. `init --adopt` runs
/// cargo-workspace's activate hook, which shells out to
/// `cargo generate-lockfile`; without cargo the adopt cannot complete, so
/// there is no successful run to assert the no-clobber claim against.
macro_rules! require_cargo {
    () => {
        if which::which("cargo").is_err() {
            eprintln!("skipping test: `cargo` not found on PATH");
            return;
        }
    };
}

/// The sub-packages the fixture's `org/lib` repo contributes. `legacy` exists
/// on disk and is listed by the committed manifest, but is deliberately absent
/// from the `include:` list in `rwv.toml` — it is the member an authoring pass
/// would drop.
const LIB_SUBCRATES: &[&str] = &["core", "cli", "legacy"];

/// `rwv.toml` for a project whose single repo contributes sub-path members.
///
/// `org/lib` is declared but has no root `Cargo.toml`, so it is invisible to
/// `detect_repos_with_manifest` — the `members:` block is the only reason
/// cargo-workspace considers this project to have active cargo work.
const CARGO_MEMBERS_MANIFEST: &str = "[repositories.\"org/lib\"]\ntype = \"git\"\nurl = \"https://example.com/lib.git\"\nversion = \"main\"\nrole = \"owned\"\n\n[integrations.cargo-workspace.members.\"org/lib\"]\ninclude = [\"crates/core\", \"crates/cli\"]\n";

/// A bare project repo carrying [`CARGO_MEMBERS_MANIFEST`] and a committed
/// root `Cargo.toml`, ready to be adopted.
///
/// `cargo_lock` commits a `Cargo.lock` alongside it. `None` is the shape the
/// no-clobber tests want (the lock is absent, so nothing about it can be
/// asserted); `Some` is for
/// [`init_adopt_regenerates_a_committed_cargo_lock`], where the committed lock
/// is the subject.
fn make_cargo_adoptee_bare(
    tmp: &Path,
    name: &str,
    cargo_toml: &str,
    cargo_lock: Option<&str>,
) -> PathBuf {
    let bare = tmp.join(format!("{name}.git"));
    init_bare_repo(&bare);
    let work = tmp.join(format!("{name}-work"));
    git_run(
        &["clone", &bare.to_string_lossy(), &work.to_string_lossy()],
        tmp,
    );
    std::fs::write(work.join("rwv.toml"), CARGO_MEMBERS_MANIFEST).unwrap();
    std::fs::write(work.join("Cargo.toml"), cargo_toml).unwrap();
    if let Some(lock) = cargo_lock {
        std::fs::write(work.join("Cargo.lock"), lock).unwrap();
    }
    git_run(&["add", "."], &work);
    git_run(&["commit", "-m", "manifest + managed Cargo.toml"], &work);
    git_run(&["push", "origin", "main"], &work);
    bare
}

/// Materialize `org/lib` in the adopting workspace: no root `Cargo.toml`,
/// one package per entry in `subcrates` under `crates/`.
fn materialize_lib_subcrates(ws: &Path, subcrates: &[&str]) {
    for name in subcrates {
        let dir = ws.join("org/lib/crates").join(name);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            format!("[package]\nname = \"lib-{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        )
        .unwrap();
        std::fs::write(dir.join("src/lib.rs"), "").unwrap();
    }
}

/// The consequential form of the adopt-clobber bug: an authoring pass at
/// adopt time rewrites `[workspace].members` from the config, dropping the
/// member the committed file names.
#[test]
fn init_adopt_does_not_truncate_committed_cargo_members() {
    require_cargo!();
    let tmp = common::tempdir().unwrap();

    // Marked, committed manifest listing three members. `rwv.toml`'s
    // `include:` names only two, so authoring computes a strictly smaller
    // set — the truncation. `[profile.release]` is user policy that must
    // survive either way, so it cannot mask the members difference.
    let committed_cargo_toml = "\
[workspace]
# managed by rwv
members = [\"org/lib/crates/cli\", \"org/lib/crates/core\", \"org/lib/crates/legacy\"]
resolver = \"2\"

[profile.release]
lto = true
";
    let bare = make_cargo_adoptee_bare(tmp.path(), "myapp", committed_cargo_toml, None);

    let adopt_ws = make_workspace(tmp.path());
    materialize_lib_subcrates(&adopt_ws, LIB_SUBCRATES);
    let source = common::file_url(&bare);

    rwv()
        .args(["init", "--adopt", &source])
        .current_dir(&adopt_ws)
        .assert()
        .success();

    let adopted = std::fs::read_to_string(adopt_ws.join("projects/myapp/Cargo.toml"))
        .expect("adopted project should still carry the committed Cargo.toml");
    assert!(
        adopted.contains("org/lib/crates/legacy"),
        "`rwv init --adopt` must not drop a committed workspace member; \
         authoring at adopt time truncates [workspace].members to what the \
         config yields, which silently shrinks the workspace. Got:\n{adopted}"
    );
    assert_eq!(
        adopted, committed_cargo_toml,
        "`rwv init --adopt` is a context verb: it must leave the adopted \
         repo's committed Cargo.toml byte-for-byte alone"
    );
}

/// The unmarked half. `merge_activate` defers the `Author` key `members` when
/// the marker is absent, so the members axis cannot see an authoring pass
/// here — but it still sets the `DefaultOnly` key `resolver` (absent → write)
/// and serializes the file back. A context verb writes neither.
#[test]
fn init_adopt_does_not_write_into_a_user_held_cargo_manifest() {
    require_cargo!();
    let tmp = common::tempdir().unwrap();

    // No marker, and members that already agree with the config — so the
    // only thing an authoring pass changes is the injected `resolver`.
    let committed_cargo_toml = "\
[workspace]
members = [\"org/lib/crates/cli\", \"org/lib/crates/core\"]

[profile.release]
lto = true
";
    let bare = make_cargo_adoptee_bare(tmp.path(), "myapp", committed_cargo_toml, None);

    let adopt_ws = make_workspace(tmp.path());
    materialize_lib_subcrates(&adopt_ws, &["core", "cli"]);
    let source = common::file_url(&bare);

    rwv()
        .args(["init", "--adopt", &source])
        .current_dir(&adopt_ws)
        .assert()
        .success();

    let adopted = std::fs::read_to_string(adopt_ws.join("projects/myapp/Cargo.toml"))
        .expect("adopted project should still carry the committed Cargo.toml");
    assert!(
        !adopted.contains("resolver"),
        "`rwv init --adopt` must not inject the DefaultOnly `resolver` key \
         into a Cargo.toml the user holds the pen on. Got:\n{adopted}"
    );
    assert_eq!(
        adopted, committed_cargo_toml,
        "`rwv init --adopt` must leave a user-held Cargo.toml byte-for-byte alone"
    );
}

// ===========================================================================
// The other half of the ownership distinction: install hooks DO rewrite the
// lockfiles they own, at context verbs, committed or not
//
// The three tests above pin the authoring path. This section pins the hook
// path, which the same adopt runs and which is NOT subject to "never author"
// — see docs/explanation/joints/file-ownership.md §"Install hooks at context
// verbs: lockfiles may be rewritten". Without a test in this direction the
// behaviour is unexercised both ways, which is the state most likely to
// change by accident: a future reader who takes "adopt never re-authors
// committed content" as unscoped would suppress the hook and break nothing
// visible.
// ===========================================================================

/// A committed `Cargo.lock` in valid lock format whose `lib-stale-sentinel`
/// entry regeneration cannot produce — no member is named that and there are
/// no external deps to pull one in. Its survival is the discriminator: bytes
/// preserved means the hook did not run.
const STALE_COMMITTED_CARGO_LOCK: &str = "\
# This file is automatically @generated by Cargo.
# It is not intended for manual editing.
version = 4

[[package]]
name = \"lib-core\"
version = \"0.1.0\"

[[package]]
name = \"lib-stale-sentinel\"
version = \"0.0.9\"
";

/// Adopt regenerates a committed `Cargo.lock`. Ruled correct under the shipped
/// ownership model: a generated lock is fully rwv-owned derived state, and
/// committing one does not transfer the pen.
///
/// This pins the DEFAULT lock policy — the only one that exists in code today.
/// The opt-in `commit-lock: true` policy, when it ships, adds a SEPARATE
/// knob-set fixture pinning non-clobber; this test stays as the default-column
/// pin and must not be weakened to accommodate it.
///
/// Every member resolves here on purpose: the members-missing case is the
/// subject of the next test, and mixing the two would let either one explain a
/// pass.
#[test]
fn init_adopt_regenerates_a_committed_cargo_lock() {
    require_cargo!();
    let tmp = common::tempdir().unwrap();

    // Marked, and members that already agree with the config — no truncation
    // axis in play, so the lock is the only thing under test.
    let committed_cargo_toml = "\
[workspace]
# managed by rwv
members = [\"org/lib/crates/cli\", \"org/lib/crates/core\"]
resolver = \"2\"
";
    let bare = make_cargo_adoptee_bare(
        tmp.path(),
        "myapp",
        committed_cargo_toml,
        Some(STALE_COMMITTED_CARGO_LOCK),
    );

    let adopt_ws = make_workspace(tmp.path());
    materialize_lib_subcrates(&adopt_ws, &["core", "cli"]);
    let source = common::file_url(&bare);

    rwv()
        .args(["init", "--adopt", &source])
        .current_dir(&adopt_ws)
        .assert()
        .success();

    let lock = std::fs::read_to_string(adopt_ws.join("projects/myapp/Cargo.lock"))
        .expect("adopt should leave a Cargo.lock at the canonical project path");
    assert!(
        !lock.contains("lib-stale-sentinel"),
        "`rwv init --adopt` runs the cargo install hook, which regenerates the \
         lockfile it owns — the committed bytes must not survive. Got:\n{lock}"
    );
    assert!(
        lock.contains("name = \"lib-cli\""),
        "the lock must be REPLACED by a real regeneration, not emptied or \
         deleted: every workspace member should appear in it. Got:\n{lock}"
    );
}

/// Adopt of a cargo project whose members are not fetched yet completes.
///
/// `init --adopt` clones only the project repo, so a manifest that already
/// names its members names paths that are not on disk — the normal starting
/// state for adopting any existing cargo workspace, not an edge case. Left to
/// cargo this is exit 101, which takes the whole adopt down with it after it
/// has otherwise succeeded.
///
/// Deferring is not suppression: the test above pins that the hook still runs
/// and still rewrites the lock once the members resolve.
#[test]
fn init_adopt_completes_when_workspace_members_are_not_fetched_yet() {
    require_cargo!();
    let tmp = common::tempdir().unwrap();

    let committed_cargo_toml = "\
[workspace]
# managed by rwv
members = [\"org/lib/crates/cli\", \"org/lib/crates/core\"]
resolver = \"2\"
";
    let bare = make_cargo_adoptee_bare(tmp.path(), "myapp", committed_cargo_toml, None);

    // The point of the fixture: `materialize_lib_subcrates` is NOT called, so
    // neither member path exists.
    let adopt_ws = make_workspace(tmp.path());
    let source = common::file_url(&bare);

    let out = rwv()
        .args(["init", "--adopt", &source])
        .current_dir(&adopt_ws)
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    // Naming the members and the resolving command is half the acceptance:
    // cargo's raw exit-101 text is not an acceptable user-facing message.
    for member in ["org/lib/crates/cli", "org/lib/crates/core"] {
        assert!(
            stderr.contains(member),
            "the skip must name the unfetched member `{member}` so the operator \
             knows which paths are missing. Got:\n{stderr}"
        );
    }
    assert!(
        stderr.contains("rwv fetch") && stderr.contains("rwv activate"),
        "the skip must name the commands that resolve it. `rwv fetch` alone \
         does not: the adopt wrote `.rwv-active`, so fetch skips its \
         first-fetch auto-activate and the lock is generated only by the \
         explicit `rwv activate` that follows. Got:\n{stderr}"
    );

    // The adopt is otherwise complete — the lock is the only thing deferred.
    assert!(
        adopt_ws.join(".rwv-active").exists(),
        "a completed adopt selects the project it adopted"
    );
    let adopted = std::fs::read_to_string(adopt_ws.join("projects/myapp/Cargo.toml"))
        .expect("adopted project should still carry the committed Cargo.toml");
    assert_eq!(
        adopted, committed_cargo_toml,
        "deferring the lockfile must not disturb the committed Cargo.toml"
    );
}
