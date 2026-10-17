//! `rwv fetch <source>` and `rwv init --adopt` both bring an existing project
//! repo under rwv's management from the outside, rather than minting one the
//! way `rwv init` does. Either path must leave `merge.rwv-ours.driver`
//! configured in that project repo's own `.git/config` whenever its
//! `.gitattributes` assigns `rwv.lock merge=rwv-ours` — the rename alone does
//! nothing until some config defines the driver by name (`src/git.rs`,
//! `plant_rwv_merge_driver_config`).
//!
//! `init --adopt` always calls `set_replay_exclusion` before planting, so the
//! declaration is guaranteed by the time it plants — that path is pinned here
//! as a regression guard, not a fix. `fetch` never writes that declaration
//! (it is cloning a project that already exists; whether the lock is derived
//! is that project's own prior choice), so it must check rather than force,
//! and the negative case below is the one that actually distinguishes the
//! fetch behavior from a blind, unconditional plant.

use std::path::Path;
use std::process;

mod common;

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

fn run(args: &[&str], cwd: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(cwd)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git command failed to start");
    assert!(status.success(), "git {:?} failed", args);
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

/// Push a project source (an `rwv.toml`-carrying repo `rwv fetch` can clone)
/// to a fresh bare repo, optionally declaring the `rwv.lock merge=rwv-ours`
/// replay exclusion the way `rwv init` writes it for its own projects.
fn push_project_source(tmp: &Path, name: &str, declare_merge_driver: bool) -> std::path::PathBuf {
    let bare = tmp.join(format!("{name}.git"));
    init_bare_repo(&bare);

    let work = tmp.join(format!("{name}-work"));
    run(
        &["clone", &bare.to_string_lossy(), &work.to_string_lossy()],
        tmp,
    );
    run(&["config", "user.email", "test@test.com"], &work);
    run(&["config", "user.name", "Test"], &work);

    std::fs::write(work.join("rwv.toml"), "[repositories]\n").unwrap();
    run(&["add", "rwv.toml"], &work);
    if declare_merge_driver {
        std::fs::write(work.join(".gitattributes"), "rwv.lock merge=rwv-ours\n").unwrap();
        run(&["add", ".gitattributes"], &work);
    }
    run(&["commit", "-m", "project source"], &work);
    run(&["push", "origin", "main"], &work);
    bare
}

/// `git config --get <key>` in `repo`; `None` when the key is unset.
fn git_config_get(repo: &Path, key: &str) -> Option<String> {
    let out = common::git()
        .args(["config", "--get", key])
        .current_dir(repo)
        .output()
        .unwrap();
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

fn doctor_violations(root: &Path) -> Vec<serde_json::Value> {
    let out = rwv()
        .args(["doctor", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("doctor --json must emit valid JSON");
    json["violations"].as_array().cloned().unwrap_or_default()
}

#[test]
fn fetch_plants_merge_driver_config_when_gitattributes_declares_it() {
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let bare = push_project_source(tmp.path(), "with-driver", true);

    rwv()
        .args(["fetch", &common::file_url(&bare)])
        .current_dir(&workspace)
        .assert()
        .success();

    let project_dir = workspace.join("projects/with-driver");
    assert_eq!(
        git_config_get(&project_dir, "merge.rwv-ours.driver").as_deref(),
        Some("true"),
        "fetch should plant merge.rwv-ours.driver when .gitattributes already \
         declares rwv.lock merge=rwv-ours"
    );

    let violations = doctor_violations(&workspace);
    assert!(
        !violations
            .iter()
            .any(|v| v["kind"] == "missing-merge-driver-config"),
        "doctor should report no missing-merge-driver-config after fetch; got {violations:#?}"
    );
}

#[test]
fn fetch_leaves_merge_driver_unconfigured_without_gitattributes_declaration() {
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let bare = push_project_source(tmp.path(), "no-driver", false);

    rwv()
        .args(["fetch", &common::file_url(&bare)])
        .current_dir(&workspace)
        .assert()
        .success();

    let project_dir = workspace.join("projects/no-driver");
    assert_eq!(
        git_config_get(&project_dir, "merge.rwv-ours.driver"),
        None,
        "fetch must not plant merge.rwv-ours.driver when .gitattributes never \
         declared rwv.lock merge=rwv-ours"
    );
}

#[test]
fn adopt_plants_merge_driver_config_for_the_adopted_project_repo() {
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(workspace.join("github")).unwrap();
    std::fs::create_dir_all(workspace.join("projects")).unwrap();

    // A plain repo with no rwv.toml and no .gitattributes — `init --adopt`
    // mints both the declaration and the config for a project it is bringing
    // under management for the first time, unlike `fetch`.
    let bare = tmp.path().join("adoptee.git");
    init_bare_repo(&bare);
    let work = tmp.path().join("adoptee-work");
    run(
        &["clone", &bare.to_string_lossy(), &work.to_string_lossy()],
        tmp.path(),
    );
    run(&["config", "user.email", "test@test.com"], &work);
    run(&["config", "user.name", "Test"], &work);
    std::fs::write(work.join("README.md"), "# adoptee\n").unwrap();
    run(&["add", "."], &work);
    run(&["commit", "-m", "initial"], &work);
    run(&["push", "origin", "main"], &work);

    rwv()
        .args(["init", "--adopt", &common::file_url(&bare)])
        .current_dir(&workspace)
        .assert()
        .success();

    let project_dir = workspace.join("projects/adoptee");
    assert_eq!(
        git_config_get(&project_dir, "merge.rwv-ours.driver").as_deref(),
        Some("true"),
        "init --adopt should plant merge.rwv-ours.driver alongside the \
         .gitattributes declaration it writes for the adopted project"
    );

    let violations = doctor_violations(&workspace);
    assert!(
        !violations
            .iter()
            .any(|v| v["kind"] == "missing-merge-driver-config"),
        "doctor should report no missing-merge-driver-config after adopt; got {violations:#?}"
    );
}
