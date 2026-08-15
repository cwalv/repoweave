//! `rwv init <name>` performs the first surfacing pass for a project,
//! materializing the weave-root `<name>.code-workspace` symlink. Any later
//! surfacing pass — `rwv add`, `rwv doctor --fix` — used to fail whenever
//! `<name>` carried more than one path segment: the removal step that should
//! recognize the symlink as rwv's own prior surfacing never did, so the
//! unconditional recreate that follows collided with it (EEXIST).
//!
//! Every existing surfacing fixture in this suite names its project with one
//! path segment (`web-app`, `myproj`, `test-project`, ...). Production gives
//! no such guarantee — `rwv init acme/console` is exactly as legal as
//! `rwv init console` — so nothing here had ever driven a second surfacing
//! pass against a multi-segment name until now. The name below carries no
//! `projects` segment at all, so a fix scoped to that one word would still
//! leave this red.

mod common;

use std::path::Path;
use std::process;

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(status.success(), "git {args:?} in {} failed", dir.display());
}

/// A bare repo with one commit on `main`, so it can be cloned or added.
fn init_bare_repo_with_commit(bare: &Path) {
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    git(
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            &bare.to_string_lossy(),
        ],
        bare.parent().unwrap(),
    );
    let seed = bare.with_extension("seed");
    git(
        &["clone", &bare.to_string_lossy(), &seed.to_string_lossy()],
        bare.parent().unwrap(),
    );
    git(&["config", "user.email", "test@test.com"], &seed);
    git(&["config", "user.name", "Test"], &seed);
    std::fs::write(seed.join("README"), "seed").unwrap();
    git(&["add", "."], &seed);
    git(&["commit", "-m", "initial"], &seed);
    git(&["push", "origin", "main"], &seed);
}

#[test]
fn add_succeeds_after_init_for_a_multi_segment_project_name() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    rwv()
        .args(["init", "acme/console"])
        .current_dir(&ws)
        .assert()
        .success();

    let project_dir = ws.join("projects/acme/console");
    git(&["config", "user.email", "test@test.com"], &project_dir);
    git(&["config", "user.name", "Test"], &project_dir);
    git(&["add", "-A"], &project_dir);
    git(&["commit", "-m", "initial"], &project_dir);

    let bare = tmp.path().join("remote.git");
    init_bare_repo_with_commit(&bare);
    let url = common::file_url(&bare);

    // The regression: `rwv init` above already ran the FIRST surfacing pass,
    // materializing the weave-root symlink. This `add` is the second pass —
    // the one that used to exit 1 on an EEXIST it raised against its own
    // prior link.
    rwv()
        .args(["add", &url])
        .current_dir(&ws)
        .assert()
        .success();

    let link = ws.join("acme+console.code-workspace");
    let target = std::fs::read_link(&link)
        .unwrap_or_else(|e| panic!("{} should be a symlink: {e}", link.display()));
    assert_eq!(
        target,
        Path::new("projects/acme/console/acme+console.code-workspace"),
        "surfacing symlink must resolve into the project's own directory"
    );
    assert!(
        project_dir.join("acme+console.code-workspace").is_file(),
        "the symlink's target must actually exist"
    );

    // A third surfacing pass, via the local-path add form: the sibling
    // measurement (rwv-jodu) found both add forms failing identically.
    let bare2 = tmp.path().join("second.git");
    init_bare_repo_with_commit(&bare2);
    let canonical = ws.join("vendor/second");
    std::fs::create_dir_all(canonical.parent().unwrap()).unwrap();
    git(
        &[
            "clone",
            &bare2.to_string_lossy(),
            &canonical.to_string_lossy(),
        ],
        tmp.path(),
    );

    rwv()
        .args(["add", "vendor/second"])
        .current_dir(&ws)
        .assert()
        .success();

    let manifest = std::fs::read_to_string(project_dir.join("rwv.toml")).unwrap();
    assert!(manifest.contains("vendor/second"), "got:\n{manifest}");
}
