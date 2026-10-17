//! cargo-workspace's `rustc-wrapper` key tracks the host at every
//! materialization, not only when the managed files are authored: `rwv
//! materialize`, the first activation `rwv fetch` performs, and `rwv workweave
//! create` write it where the wrapper is on `PATH` and strip it where it is
//! not, and the weave root links the file in the same pass.
//!
//! Unix only: the `cargo` and `sccache` stand-ins are shebang scripts a child
//! resolves off `PATH`, the boundary `common::cargo_stand_in_path` states.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

mod common;

const WRAPPER_KEY: &str = "rustc-wrapper = \"sccache\"";

fn git_init_with_commit(dir: &Path) {
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
}

fn install_sccache(bin: &Path) {
    let sccache = bin.join("sccache");
    std::fs::write(&sccache, "#!/bin/sh\nexec \"$@\"\n").unwrap();
    std::fs::set_permissions(&sccache, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn uninstall_sccache(bin: &Path) {
    std::fs::remove_file(bin.join("sccache")).unwrap();
}

/// A bare member repo holding one crate, returned as its `file://` URL.
fn crate_remote(tmp: &Path) -> String {
    let work = tmp.join("lib-src");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(
        work.join("Cargo.toml"),
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    git_init_with_commit(&work);
    let bare = tmp.join("lib.git");
    common::git_in(
        tmp,
        &[
            "clone",
            "--bare",
            &work.to_string_lossy(),
            &bare.to_string_lossy(),
        ],
    );
    common::file_url(&bare)
}

/// A weave whose active project `app` lists the crate at `github/acme/lib`
/// and asks for the wrapper, with the member cloned and the project a repo.
fn weave(tmp: &Path, url: &str) -> PathBuf {
    let ws = tmp.join("ws");
    std::fs::create_dir_all(ws.join("projects/app")).unwrap();
    common::git_in(
        tmp,
        &["clone", url, &ws.join("github/acme/lib").to_string_lossy()],
    );
    std::fs::write(
        ws.join("projects/app/rwv.toml"),
        format!(
            "[repositories.\"github/acme/lib\"]\ntype = \"git\"\nurl = \"{url}\"\n\
             version = \"main\"\nrole = \"owned\"\n\n\
             [integrations.cargo-workspace]\nrustc-wrapper = \"sccache\"\n\n\
             [integrations.vscode-workspace]\nenabled = false\n"
        ),
    )
    .unwrap();
    git_init_with_commit(&ws.join("projects/app"));
    std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();
    ws
}

fn rwv(ws: &Path, path: &std::ffi::OsStr, args: &[&str]) -> String {
    let out = common::rwv()
        .args(args)
        .current_dir(ws)
        .env("PATH", path)
        .output()
        .expect("rwv should run");
    let report = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "`rwv {}` failed:\n{report}",
        args.join(" ")
    );
    report
}

/// The wrapper key as the weave root sees it: through a link into the project
/// copy, which is the only place cargo's upward config discovery finds it.
fn surfaced_config(ws: &Path, project: &str) -> Option<String> {
    let link = ws.join(".cargo/config.toml");
    let is_link = link
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink());
    let resolves_to_project = link.canonicalize().ok()
        == ws
            .join("projects")
            .join(project)
            .join(".cargo/config.toml")
            .canonicalize()
            .ok();
    (is_link && resolves_to_project)
        .then(|| std::fs::read_to_string(&link).ok())
        .flatten()
}

#[test]
fn materialize_fits_the_wrapper_key_to_the_host_both_ways() {
    let tmp = common::tempdir().unwrap();
    let url = crate_remote(tmp.path());
    let ws = weave(tmp.path(), &url);
    let bin = tmp.path().join("bin");
    let path = common::cargo_stand_in_path(&bin);
    let config = ws.join("projects/app/.cargo/config.toml");

    rwv(&ws, &path, &["doctor", "--fix"]);
    assert!(
        !config.exists(),
        "fixture: with no wrapper on PATH, authoring writes no wrapper key"
    );

    install_sccache(&bin);
    let report = rwv(&ws, &path, &["materialize"]);
    assert!(
        std::fs::read_to_string(&config).is_ok_and(|c| c.contains(WRAPPER_KEY)),
        "materialize must write the wrapper key once the wrapper is installed:\n{report}"
    );
    assert!(
        surfaced_config(&ws, "app").is_some_and(|c| c.contains(WRAPPER_KEY)),
        "and link it at the weave root in the same pass:\n{report}"
    );

    uninstall_sccache(&bin);
    let report = rwv(&ws, &path, &["materialize"]);
    assert!(
        !config.exists(),
        "materialize must strip the key once the wrapper is gone — cargo \
         hard-errors on a wrapper it cannot spawn:\n{report}"
    );
}

#[test]
fn fetch_fits_the_wrapper_key_at_its_first_activation() {
    let tmp = common::tempdir().unwrap();
    let url = crate_remote(tmp.path());
    let authoring = weave(tmp.path(), &url);
    let bin = tmp.path().join("bin");
    let path = common::cargo_stand_in_path(&bin);

    rwv(&authoring, &path, &["doctor", "--fix"]);
    let project = authoring.join("projects/app");
    common::git_in(&project, &["add", "-A"]);
    common::git_in(&project, &["commit", "-m", "authored"]);
    let source = tmp.path().join("app.git");
    common::git_in(
        tmp.path(),
        &[
            "clone",
            "--bare",
            &project.to_string_lossy(),
            &source.to_string_lossy(),
        ],
    );

    install_sccache(&bin);
    let fresh = tmp.path().join("fresh");
    std::fs::create_dir_all(&fresh).unwrap();
    let report = rwv(&fresh, &path, &["fetch", &common::file_url(&source)]);
    assert!(
        surfaced_config(&fresh, "app").is_some_and(|c| c.contains(WRAPPER_KEY)),
        "fetch's first activation must write the wrapper key and link it:\n{report}"
    );
}

#[test]
fn workweave_create_fits_the_wrapper_key_in_the_new_seat() {
    let tmp = common::tempdir().unwrap();
    let url = crate_remote(tmp.path());
    let ws = weave(tmp.path(), &url);
    let bin = tmp.path().join("bin");
    let path = common::cargo_stand_in_path(&bin);

    rwv(&ws, &path, &["doctor", "--fix"]);
    let project = ws.join("projects/app");
    common::git_in(&project, &["add", "-A"]);
    common::git_in(&project, &["commit", "-m", "authored"]);

    install_sccache(&bin);
    let report = rwv(&ws, &path, &["workweave", "app", "create", "seat"]);
    let seat = tmp.path().join(".workweaves/app--seat");
    assert!(
        surfaced_config(&seat, "app").is_some_and(|c| c.contains(WRAPPER_KEY)),
        "create's materialization must write the wrapper key in the seat and link it:\n{report}"
    );
}
