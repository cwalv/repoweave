//! A state file's location accepts no path-taking write.
//!
//! `StatePath` is not `AsRef<Path>`, does not deref to a path and hands none
//! out, so `std::fs::write` and every other call that takes a path refuse it
//! at compile time; the durable writes it offers are the only writes there
//! are. Pinned by diagnostic code, from an external crate, where every
//! visibility narrower than `pub` looks alike.
//!
//! Residue: `StatePath::dir` and `StateFile::file_name` are public, so a
//! caller can rebuild the path by hand. `state_file_publish_audit_test` scans
//! production source for exactly that join.

mod common;

use common::compile_probe::{assert_fails_with, compile};

#[test]
fn the_harness_can_write_a_state_path_durably() {
    // Control. Everything below asserts a failure, so a broken rustc
    // invocation would make them all pass for the wrong reason.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::state_file::StateFile;
        use std::path::Path;
        pub fn write(dir: &Path) -> bool {
            let state = StateFile::ActiveProject.in_dir(dir);
            let published = state.publish(b"web-app\n").is_ok();
            let read = state.read_to_string().is_ok();
            let shown = state.to_string();
            let wire = state.wire();
            let removed = state.remove().is_ok();
            let _ = (state.exists(), state.dir(), state.file(), wire, shown);
            published && read && removed
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn fs_write_refuses_a_state_path() {
    assert_fails_with(
        "E0277",
        "a state path is not a path std::fs::write accepts",
        r#"
        use repoweave::state_file::StateFile;
        use std::path::Path;
        pub fn write(dir: &Path) -> std::io::Result<()> {
            let state = StateFile::ProjectLock.in_dir(dir);
            std::fs::write(state, b"torn")
        }
        "#,
    );
}

#[test]
fn fs_write_refuses_a_borrowed_state_path() {
    assert_fails_with(
        "E0277",
        "borrowing it does not make it a path either",
        r#"
        use repoweave::state_file::StateFile;
        use std::path::Path;
        pub fn write(dir: &Path) -> std::io::Result<()> {
            let state = StateFile::ProjectLock.in_dir(dir);
            std::fs::write(&state, b"torn")
        }
        "#,
    );
}

#[test]
fn file_create_refuses_a_state_path() {
    assert_fails_with(
        "E0277",
        "File::create truncates before it writes",
        r#"
        use repoweave::state_file::StateFile;
        use std::path::Path;
        pub fn open(dir: &Path) -> std::io::Result<std::fs::File> {
            std::fs::File::create(StateFile::HealthFloor.in_dir(dir))
        }
        "#,
    );
}

#[test]
fn open_options_refuse_a_state_path() {
    assert_fails_with(
        "E0277",
        "an append or write open of a state file is a path-taking write",
        r#"
        use repoweave::state_file::StateFile;
        use std::path::Path;
        pub fn open(dir: &Path) -> std::io::Result<std::fs::File> {
            std::fs::OpenOptions::new()
                .write(true)
                .open(StateFile::WorkweaveIndex.in_dir(dir))
        }
        "#,
    );
}

#[test]
fn a_rename_or_copy_onto_a_state_path_is_refused() {
    assert_fails_with(
        "E0277",
        "replacing a state file by rename bypasses the fsyncs",
        r#"
        use repoweave::state_file::StateFile;
        use std::path::Path;
        pub fn replace(from: &Path, dir: &Path) -> std::io::Result<()> {
            std::fs::rename(from, StateFile::WorkweaveMarker.in_dir(dir))
        }
        "#,
    );
    assert_fails_with(
        "E0277",
        "so does a copy",
        r#"
        use repoweave::state_file::StateFile;
        use std::path::Path;
        pub fn replace(from: &Path, dir: &Path) -> std::io::Result<u64> {
            std::fs::copy(from, StateFile::OwnedDigests.in_dir(dir))
        }
        "#,
    );
}

#[test]
fn a_state_path_does_not_coerce_to_a_path() {
    assert_fails_with(
        "E0308",
        "there is no Deref to Path",
        r#"
        use repoweave::state_file::StateFile;
        use std::path::Path;
        pub fn as_path(dir: &Path) -> &Path {
            let state = StateFile::ActiveProject.in_dir(dir);
            &state
        }
        "#,
    );
}

#[test]
fn a_state_path_is_not_converted_into_a_path_buf() {
    assert_fails_with(
        "E0277",
        "there is no From<StatePath> for PathBuf",
        r#"
        use repoweave::state_file::StateFile;
        use std::path::{Path, PathBuf};
        pub fn into_path(dir: &Path) -> PathBuf {
            PathBuf::from(StateFile::ActiveProject.in_dir(dir))
        }
        "#,
    );
}

#[test]
fn a_state_path_has_no_path_methods() {
    assert_fails_with(
        "E0599",
        "no join, no parent, no to_path_buf",
        r#"
        use repoweave::state_file::StateFile;
        use std::path::{Path, PathBuf};
        pub fn rebuild(dir: &Path) -> PathBuf {
            StateFile::ActiveProject.in_dir(dir).to_path_buf()
        }
        "#,
    );
}

#[test]
fn the_exits_and_exclusive_writes_are_crate_private() {
    assert_fails_with(
        "E0624",
        "the report exit is not visible outside the crate",
        r#"
        use repoweave::state_file::StateFile;
        use std::path::{Path, PathBuf};
        pub fn report(dir: &Path) -> PathBuf {
            StateFile::ActiveProject.in_dir(dir).reported()
        }
        "#,
    );
    assert_fails_with(
        "E0624",
        "nor is the exclusive create",
        r#"
        use repoweave::state_file::StateFile;
        use std::path::Path;
        pub fn create(dir: &Path) {
            let _ = StateFile::OpRecord.in_dir(dir).create(b"op");
        }
        "#,
    );
}

#[test]
fn a_state_path_cannot_be_built_from_its_fields() {
    assert_fails_with(
        "E0451",
        "the fields are private to state_file",
        r#"
        use repoweave::state_file::{StateFile, StatePath};
        use std::path::PathBuf;
        pub fn forge(dir: PathBuf) -> StatePath {
            StatePath { dir, file: StateFile::ActiveProject }
        }
        "#,
    );
}
