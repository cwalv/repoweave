//! Regression tests: `rwv workweave create` forked only what git tracks, so
//! a managed file that is deliberately untracked — cargo-workspace's
//! gitignored `.cargo/config.toml`, under `rustc-wrapper` — started every
//! fresh seat missing, even when the source workspace had already activated
//! it. `rwv doctor --fix` regenerated it immediately afterward, which is
//! what made the gap easy to miss and annoying to live with.
//!
//! The fix carries bytes, not authorship: during `create_workweave`, before
//! the verify pass, a declared managed/generated file is copied from the
//! source into the seat when the seat lacks it, the source has it as a
//! regular file, and the seat's project VCS does not track it. Workweave
//! creation still never calls `Integration::activate()` — see
//! `workweave_create_does_not_author_managed_content` in
//! `doc_claims_intent_verbs_test.rs`, pinned unchanged.
//!
//! Three cases, one fixture shape (one rust crate, `cargo-workspace` with
//! `rustc-wrapper = "sccache"`):
//!   (a) the source activated: the seat gets the same bytes, no drift left.
//!   (b) the source never activated: the seat stays empty-handed too.
//!   (d) the file is tracked (operator committed it): a dirty local edit at
//!       the source never reaches the seat, which keeps the checked-out,
//!       committed bytes.
//!
//! `sccache` only needs to resolve on `PATH` — `detected_wrapper` is a
//! `which::which` lookup, nothing here ever runs it — so the stand-in is a
//! single `exec "$@"` script. Unix only: the executable bit it needs has no
//! Windows equivalent, the same boundary `common::cargo_stand_in_path` states.

#![cfg(unix)]

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

mod common;

/// Skip if `cargo` is not on `PATH` — `rwv doctor --fix` runs cargo-workspace's
/// activate hook (`cargo generate-lockfile`) alongside the content-authoring
/// step this file is actually about, and a missing `cargo` there is a
/// separate, unrelated failure this file does not mean to exercise.
macro_rules! require_cargo {
    () => {
        if common::skip_without_tool("cargo") {
            return;
        }
    };
}

fn git_init_with_commit(dir: &Path) {
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
}

/// `PATH` prepended with a directory holding only a `sccache` stand-in, so
/// `which::which("sccache")` resolves without the host needing the real
/// tool. The real `PATH` stays behind it — `cargo`, `git` and everything
/// else `rwv` shells out to must still resolve.
fn path_with_sccache_stand_in(bin: &Path) -> OsString {
    std::fs::create_dir_all(bin).expect("stand-in bin dir should be creatable");
    let sccache = bin.join("sccache");
    std::fs::write(&sccache, "#!/bin/sh\nexec \"$@\"\n").expect("stand-in should be writable");
    std::fs::set_permissions(&sccache, std::fs::Permissions::from_mode(0o755))
        .expect("stand-in should be executable");

    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut joined = bin.as_os_str().to_owned();
    joined.push(":");
    joined.push(existing);
    joined
}

/// One rust crate, a project configured to write `rustc-wrapper = "sccache"`
/// into `.cargo/config.toml`, gitignoring the files that activation would
/// generate. Returns `(ws, project_dir)`.
fn fixture(tmp: &Path) -> (PathBuf, PathBuf) {
    let ws = tmp.join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();

    let rust = ws.join("github/acme/rust-lib");
    std::fs::create_dir_all(rust.join("src")).unwrap();
    std::fs::write(
        rust.join("Cargo.toml"),
        "[package]\nname = \"rust-lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(rust.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    git_init_with_commit(&rust);

    let project_dir = ws.join("projects/web-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        "[repositories.\"github/acme/rust-lib\"]\ntype = \"git\"\n\
         url = \"https://github.com/acme/rust-lib.git\"\nversion = \"main\"\nrole = \"owned\"\n\n\
         [integrations.cargo-workspace]\nrustc-wrapper = \"sccache\"\n",
    )
    .unwrap();
    std::fs::write(
        project_dir.join(".gitignore"),
        "/Cargo.lock\n/.cargo/config.toml\n",
    )
    .unwrap();
    git_init_with_commit(&project_dir);
    std::fs::write(ws.join(".rwv-active"), "web-app\n").unwrap();

    (ws, project_dir)
}

fn rwv_with_path(args: &[&str], cwd: &Path, path: &std::ffi::OsStr) -> String {
    let output = common::rwv()
        .args(args)
        .current_dir(cwd)
        .env("PATH", path)
        .output()
        .expect("rwv should run");
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// (a) The source activated: the seat carries the same `.cargo/config.toml`
/// bytes, and nothing is left for `doctor` to report.
#[test]
fn workweave_create_carries_an_activated_wrapper_config() {
    require_cargo!();
    let tmp = common::tempdir().unwrap();
    let (ws, project_dir) = fixture(tmp.path());
    let path = path_with_sccache_stand_in(&tmp.path().join("bin"));

    rwv_with_path(&["doctor", "--fix"], &ws, &path);
    let primary_config = project_dir.join(".cargo/config.toml");
    assert!(
        primary_config.is_file(),
        "fixture: primary should have authored .cargo/config.toml"
    );
    let primary_bytes = std::fs::read(&primary_config).unwrap();
    assert!(
        String::from_utf8_lossy(&primary_bytes).contains("rustc-wrapper = \"sccache\""),
        "fixture: the authored config should carry the wrapper key"
    );
    common::git_in(&project_dir, &["add", "-A"]);
    common::git_in(&project_dir, &["commit", "-m", "activate"]);

    let create = rwv_with_path(
        &["workweave", "web-app", "create", "agent-1"],
        &ws,
        &path,
    );
    assert!(
        !create.contains("cargo-workspace managed file missing"),
        "create should leave nothing for cargo-workspace to warn about.\n{create}"
    );

    let seat = tmp.path().join(".workweaves/web-app--agent-1");
    let seat_config = seat.join("projects/web-app/.cargo/config.toml");
    assert!(
        seat_config.is_file(),
        "the seat should carry the source's .cargo/config.toml"
    );
    assert_eq!(
        std::fs::read(&seat_config).unwrap(),
        primary_bytes,
        "the carried bytes should match the source's exactly"
    );

    let report = rwv_with_path(&["doctor"], &seat, &path);
    assert!(
        !report.contains("managed file missing"),
        "doctor in the seat should report no drift.\n{report}"
    );
}

/// (b) The source never activated: the seat has nothing to carry and stays
/// exactly as empty-handed. Pins "create never writes" for this file.
#[test]
fn workweave_create_leaves_an_unactivated_wrapper_config_missing_in_both() {
    require_cargo!();
    let tmp = common::tempdir().unwrap();
    let (ws, _project_dir) = fixture(tmp.path());
    let path = path_with_sccache_stand_in(&tmp.path().join("bin"));

    // No `doctor --fix` here — the source is a freshly written rwv.toml with
    // nothing ever authored into it.
    rwv_with_path(&["workweave", "web-app", "create", "agent-1"], &ws, &path);

    let seat = tmp.path().join(".workweaves/web-app--agent-1");
    assert!(
        !seat.join("projects/web-app/.cargo/config.toml").exists(),
        "an unactivated source carries nothing, so the seat has no config.toml either"
    );

    let report = rwv_with_path(&["doctor"], &seat, &path);
    assert!(
        report.contains("cargo-workspace managed file missing"),
        "doctor in the seat should report the same absence the source has.\n{report}"
    );
}

/// (d) The operator committed `.cargo/config.toml` instead of gitignoring
/// it. A dirty, uncommitted local edit at the source must not reach the
/// seat: the worktree checkout already holds the tracked, committed bytes,
/// and the carry must not overwrite a file the VCS already tracks.
#[test]
fn workweave_create_does_not_overwrite_a_tracked_wrapper_config() {
    require_cargo!();
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    let path = path_with_sccache_stand_in(&tmp.path().join("bin"));

    let rust = ws.join("github/acme/rust-lib");
    std::fs::create_dir_all(rust.join("src")).unwrap();
    std::fs::write(
        rust.join("Cargo.toml"),
        "[package]\nname = \"rust-lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(rust.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    git_init_with_commit(&rust);

    let project_dir = ws.join("projects/web-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        "[repositories.\"github/acme/rust-lib\"]\ntype = \"git\"\n\
         url = \"https://github.com/acme/rust-lib.git\"\nversion = \"main\"\nrole = \"owned\"\n\n\
         [integrations.cargo-workspace]\nrustc-wrapper = \"sccache\"\n",
    )
    .unwrap();
    // No `.gitignore` entry for `.cargo/config.toml` here — the operator's
    // choice to commit it, which `cargo-workspace`'s own doc names as a
    // supported policy.
    std::fs::write(project_dir.join(".gitignore"), "/Cargo.lock\n").unwrap();
    git_init_with_commit(&project_dir);
    std::fs::write(ws.join(".rwv-active"), "web-app\n").unwrap();

    rwv_with_path(&["doctor", "--fix"], &ws, &path);
    let config = project_dir.join(".cargo/config.toml");
    assert!(config.is_file(), "fixture: activation should have authored it");
    common::git_in(&project_dir, &["add", "-A"]);
    common::git_in(&project_dir, &["commit", "-m", "activate"]);
    let tracked_bytes = std::fs::read(&config).unwrap();

    // Dirty, uncommitted edit — never committed, so the worktree checkout
    // below cannot see it.
    std::fs::write(&config, "# an operator's local, uncommitted edit\n").unwrap();

    let create = rwv_with_path(
        &[
            "workweave",
            "web-app",
            "create",
            "agent-1",
            "--capture-dirty",
        ],
        &ws,
        &path,
    );

    let seat = tmp.path().join(".workweaves/web-app--agent-1");
    let seat_config = seat.join("projects/web-app/.cargo/config.toml");
    assert!(
        seat_config.is_file(),
        "fixture: workweave create failed:\n{create}"
    );
    assert_eq!(
        std::fs::read(&seat_config).unwrap(),
        tracked_bytes,
        "the seat should hold the tracked, committed bytes, not the source's \
         dirty uncommitted edit — a tracked file is never a carry target"
    );
}
