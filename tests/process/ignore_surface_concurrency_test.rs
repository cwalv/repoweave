//! A ledger stamp keeps every ignore line a peer wrote, and an index write
//! adds none.
//!
//! The owned-digest ledger is a file of `projects/<p>/`, so its VCS-hygiene
//! appends land on the project's ignore surface — `.git/info/exclude` when the
//! project is a git checkout. The workweave index lives in the clone's shared
//! directory, outside the working tree, and needs no ignore line. A surface
//! republished whole-file from a read that predates a peer's append drops the
//! peer's names with both writers exiting zero.
//!
//! **What is pinned, and why it needs no concurrency.** Losing a peer's line
//! needs a writer that replaces the surface from a stale read. An append never
//! replaces it: the bytes already there stay where they are and the file stays
//! the same file. So the test seeds the surface with a line a peer wrote,
//! holds an open handle to that file, runs an index write and a ledger stamp,
//! and requires that
//!
//! - the bytes that were there are still a prefix of what the handle reads;
//! - every name the ledger contributes is present in what the handle reads;
//! - the index write contributed no name of its own.
//!
//! The handle names the file that existed before the writers ran, so a writer
//! that republishes the surface as a new file leaves the handle reading the
//! stale original, without the names it added.
//!
//! Each holds under every scheduling of the writers, because it is a property
//! of one writer's effect on the surface, not of how two writers interleave.
//! An in-place truncating rewrite keeps the file's identity and is not caught
//! here.

use crate::common;

use repoweave::manifest::{ProjectName, WorkweaveName};
use repoweave::owned_state::stamp_owned_digest;
use repoweave::path_spelling::CanonicalPath;
use repoweave::workweave_index::record_workweave;
use std::io::Read;

const PROJECT: &str = "web-app";

/// Every name the ledger contributes to the shared surface.
const EXPECTED_LINES: [&str; 2] = [".rwv-owned-digests", ".rwv-owned-digests.lock"];

const PEER_LINE: &str = "# a line a peer wrote\n";

#[test]
fn hygiene_appends_extend_the_surface_in_place_and_keep_a_peers_line() {
    let tmp = common::tempdir().expect("tempdir");
    let root = tmp.path().join("primary");
    let project_dir = root.join("projects").join(PROJECT);
    std::fs::create_dir_all(&project_dir).expect("project dir");
    common::git_in(&project_dir, &["init"]);
    let store = root.join("store");
    std::fs::create_dir_all(&store).expect("store dir");

    let exclude = project_dir.join(".git").join("info").join("exclude");
    std::fs::create_dir_all(exclude.parent().expect("info dir")).expect("info dir");
    std::fs::write(&exclude, PEER_LINE).expect("seed the peer's line");
    let mut held = std::fs::File::open(&exclude).expect("open handle on the surface");

    let project = ProjectName::new(PROJECT).expect("project name");
    record_workweave(
        &root,
        &project,
        &WorkweaveName::new("seat").expect("name"),
        CanonicalPath::of(&store),
        CanonicalPath::of(&root),
    )
    .expect("the index write succeeds");
    let mut after_index = String::new();
    std::fs::File::open(&exclude)
        .expect("reopen the surface")
        .read_to_string(&mut after_index)
        .expect("read the surface");
    assert_eq!(
        after_index, PEER_LINE,
        "an index write adds nothing to the project's ignore surface"
    );
    stamp_owned_digest(&project_dir, "Cargo.lock", b"version = 4\n")
        .expect("the ledger stamp succeeds");

    let mut through_handle = String::new();
    held.read_to_string(&mut through_handle)
        .expect("read through the held handle");
    assert!(
        through_handle.starts_with(PEER_LINE),
        "the bytes already on the surface must stay where they were: {through_handle:?}"
    );
    let present: Vec<&str> = through_handle.lines().map(str::trim).collect();
    for name in EXPECTED_LINES {
        assert!(
            present.contains(&name),
            "{name} is missing from the surface: {through_handle:?}"
        );
    }
}
