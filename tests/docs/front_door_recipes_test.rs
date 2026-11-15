//! Runs the command blocks of the front-door pages (the README quick start,
//! the tutorial, the release-line how-to) and of the how-to recipes against the
//! built binary in a hermetic sandbox. `tests/docs/front-door-recipes.sh` holds the steps and
//! checks that each one is still written in its page verbatim.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

use crate::common;

#[test]
fn front_door_command_blocks_run_verbatim() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/docs/front-door-recipes.sh");
    let bins = PathBuf::from(env!("CARGO_BIN_EXE_rwv"))
        .parent()
        .unwrap()
        .to_path_buf();
    let scratch = common::tempdir().unwrap();
    let out = Command::new("bash")
        .arg(&script)
        .arg(&bins)
        .env("TMPDIR", scratch.path())
        .output()
        .expect("bash should be available");
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "front-door recipes failed:\n{log}");
    for page in [
        "tutorial",
        "readme",
        "release-line",
        "create-a-project",
        "add-a-repo",
        "migrate",
        "bring-home",
        "each",
        "pr-review",
        "stops",
        "lineage",
    ] {
        assert!(
            log.lines().any(|line| line == format!("== {page}")),
            "{page} was not run:\n{log}"
        );
    }
}
