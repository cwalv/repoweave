//! `rwv materialize` — activation's install half, with no claim on selection.
//!
//! The verb exists because activation conflates two operations that have
//! different scopes. Selection needs a primary and can only ever name one
//! project; materialization is meaningful wherever the project identity is
//! already fixed. These tests pin the seam: the verb runs where `rwv activate`
//! is refused, it leaves selection state alone in both checkout kinds, and it
//! refuses — naming the verb that would fix it — where there is no project to
//! materialize.
//!
//! Driven through the shipped binary: the whole claim is about which verb is
//! valid in which checkout, which is a property of dispatch and workspace
//! resolution rather than of any one function.

use std::path::{Path, PathBuf};

mod common;

fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(status.success(), "git {args:?} in {} failed", dir.display());
}

fn git_init_with_commit(dir: &Path) {
    git(&["init", "--initial-branch=main"], dir);
    git(&["config", "user.email", "test@test.com"], dir);
    git(&["config", "user.name", "Test"], dir);
    git(&["add", "-A"], dir);
    git(&["commit", "-m", "init"], dir);
}

struct Fixture {
    _tmp: tempfile::TempDir,
    ws: PathBuf,
    ww: PathBuf,
}

impl Fixture {
    fn rwv(&self, args: &[&str], cwd: &Path) -> (bool, String) {
        let output = common::rwv()
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("rwv should run");
        (
            output.status.success(),
            format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        )
    }
}

/// A primary weave with one Rust member and a workweave forked off it.
///
/// The project repo gitignores the lock, so the workweave's worktree arrives
/// without one — which is what makes "the hook produced this" observable.
fn fixture() -> Fixture {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let ws = root.join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();

    let server = ws.join("github/acme/server");
    std::fs::create_dir_all(server.join("src")).unwrap();
    std::fs::write(
        server.join("Cargo.toml"),
        "[package]\nname = \"server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(server.join("src/lib.rs"), "").unwrap();
    git_init_with_commit(&server);

    let project_dir = ws.join("projects/app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        "[repositories.\"github/acme/server\"]\ntype = \"git\"\nurl = \"https://github.com/acme/server.git\"\nversion = \"main\"\nrole = \"owned\"\n",
    )
    .unwrap();
    std::fs::write(project_dir.join(".gitignore"), "/Cargo.lock\n").unwrap();
    git_init_with_commit(&project_dir);
    std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();

    // Author the managed files without materializing: the lock the tests look
    // for cannot be left over from the fixture's own setup.
    let ctx = repoweave::workspace::WorkspaceContext::resolve(&ws, None).unwrap();
    repoweave::activate::activate_intent_with_options(
        "app",
        &ctx,
        repoweave::activate::ActivateOptions {
            no_materialize: true,
        },
    )
    .expect("intent activation should succeed");
    assert!(
        !project_dir.join("Cargo.lock").exists(),
        "fixture: the setup must not leave a lock behind"
    );
    git(&["add", "-A"], &project_dir);
    git(&["commit", "-m", "activate"], &project_dir);

    let weaveroot = root.join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();
    let out = common::rwv()
        .args(["workweave", "app", "create", "agent-1"])
        .current_dir(&ws)
        .output()
        .expect("rwv should run");
    assert!(
        out.status.success(),
        "fixture: workweave create failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    Fixture {
        _tmp: tmp,
        ws,
        ww: weaveroot.join("app--agent-1"),
    }
}

/// The seam, stated as one test: the verb runs exactly where the verb it was
/// split out of is refused.
#[test]
fn materialize_runs_in_a_workweave_where_activate_is_refused() {
    if which::which("cargo").is_err() {
        eprintln!("skipping: `cargo` not found on PATH");
        return;
    }
    let f = fixture();

    let (activate_ok, activate_report) = f.rwv(&["activate", "app"], &f.ww);
    assert!(
        !activate_ok,
        "precondition: `rwv activate` is refused in a workweave:\n{activate_report}"
    );

    let lock = f.ww.join("projects/app/Cargo.lock");
    assert!(
        !lock.exists(),
        "precondition: the workweave starts without a lock"
    );

    let (ok, report) = f.rwv(&["materialize"], &f.ww);
    assert!(
        ok,
        "`rwv materialize` should succeed in a workweave:\n{report}"
    );
    assert!(
        lock.is_file(),
        "`rwv materialize` should have run the hook that produces {}:\n{report}",
        lock.display()
    );
}

/// Selection is the operation this verb does not perform. A workweave root has
/// no `.rwv-active` at all and must not acquire one; the primary's must not
/// change while a workweave materializes.
#[test]
fn materialize_leaves_selection_state_untouched() {
    if which::which("cargo").is_err() {
        eprintln!("skipping: `cargo` not found on PATH");
        return;
    }
    let f = fixture();
    let primary_pointer = f.ws.join(".rwv-active");
    let before = std::fs::read(&primary_pointer).unwrap();

    let (ok, report) = f.rwv(&["materialize"], &f.ww);
    assert!(ok, "materialize should succeed:\n{report}");
    assert!(
        !f.ww.join(".rwv-active").exists(),
        "a workweave root must not acquire a selection pointer"
    );
    assert_eq!(
        std::fs::read(&primary_pointer).unwrap(),
        before,
        "materializing a workweave must not touch primary's selection"
    );

    let (ok, report) = f.rwv(&["materialize"], &f.ws);
    assert!(ok, "materialize should succeed at primary:\n{report}");
    assert_eq!(
        std::fs::read(&primary_pointer).unwrap(),
        before,
        "materializing at primary must not rewrite the selection pointer"
    );
}

/// With no project presented there is nothing to materialize, and the refusal
/// names the verb that gives the checkout one.
#[test]
fn materialize_without_an_active_project_names_activate() {
    let f = fixture();
    std::fs::remove_file(f.ws.join(".rwv-active")).unwrap();

    let (ok, report) = f.rwv(&["materialize"], &f.ws);
    assert!(
        !ok,
        "materialize must refuse when no project is presented:\n{report}"
    );
    assert!(
        report.contains("rwv activate"),
        "the refusal must name the verb that selects a project:\n{report}"
    );
}

/// The verb takes no project name. Accepting one would make it a selection
/// verb wearing a materialize label — the exact conflation it was split out
/// of.
#[test]
fn materialize_takes_no_project_argument() {
    let f = fixture();
    let (ok, report) = f.rwv(&["materialize", "app"], &f.ws);
    assert!(!ok, "materialize must reject a project argument:\n{report}");
}
