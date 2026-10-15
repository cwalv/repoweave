//! An index write and a ledger stamp in one project keep every ignore line.
//!
//! The two records share a directory: the workweave index and the owned-digest
//! ledger are both files of `projects/<p>/`, so their VCS-hygiene appends land
//! on one ignore surface — `.git/info/exclude` when the project is a git
//! checkout. Each writer runs under the claim of its own record; nothing
//! serialises the two families against each other, and they contribute
//! different names. A surface republished whole-file from a read that predates
//! the other family's append drops that family's names with both writers
//! exiting zero.
//!
//! **What is pinned, and why it needs no concurrency.** Losing a peer's line
//! needs a writer that replaces the surface from a stale read. An append never
//! replaces it: the bytes already there stay where they are and the file stays
//! the same file. So the test seeds the surface with a line a peer wrote, holds
//! an open handle to that file, runs one writer of each family, and requires
//! that
//!
//! - the surface is still the file the handle names (a rename-over republish
//!   gives it a new identity, and the handle then reads the stale original);
//! - the bytes that were there are still a prefix of it;
//! - every name both families contribute is present.
//!
//! Each holds under every scheduling of the writers, because it is a property
//! of one writer's effect on the surface, not of how two writers interleave.
//! An in-place truncating rewrite keeps the file's identity and is not caught
//! here.

mod common;

use repoweave::manifest::{ProjectName, WorkweaveName};
use repoweave::owned_state::stamp_owned_digest;
use repoweave::vcs::{EphemeralRefName, ResolvedRevisionId};
use repoweave::workweave_index::RefRegistry;
use std::io::Read;
use std::os::unix::fs::MetadataExt;

const PROJECT: &str = "web-app";

/// Every name the two writers contribute to the shared surface: the index
/// family, then the ledger family.
const EXPECTED_LINES: [&str; 4] = [
    ".rwv-workweave-index",
    ".rwv-workweave-index.lock",
    ".rwv-owned-digests",
    ".rwv-owned-digests.lock",
];

const PEER_LINE: &str = "# a line a peer wrote\n";

#[test]
fn hygiene_appends_extend_the_surface_in_place_and_keep_a_peers_line() {
    let tmp = common::tempdir().expect("tempdir");
    let root = tmp.path().join("primary");
    let project_dir = root.join("projects").join(PROJECT);
    std::fs::create_dir_all(&project_dir).expect("project dir");
    common::git_in(&project_dir, &["init"]);
    std::fs::write(
        project_dir.join(".rwv-workweave-index"),
        format!(
            r#"{{"container":{:?},"workweaves":{{}},"receipts":[]}}"#,
            root.join(".workweaves").to_string_lossy()
        ),
    )
    .expect("seed index");
    let store = root.join("store");
    std::fs::create_dir_all(&store).expect("store dir");

    let exclude = project_dir.join(".git").join("info").join("exclude");
    std::fs::create_dir_all(exclude.parent().expect("info dir")).expect("info dir");
    std::fs::write(&exclude, PEER_LINE).expect("seed the peer's line");
    let mut held = std::fs::File::open(&exclude).expect("open handle on the surface");
    let identity = held.metadata().expect("handle metadata").ino();

    let project = ProjectName::new(PROJECT).expect("project name");
    let mut registry = RefRegistry::for_project(&root, &project);
    registry
        .record_created(
            &store,
            EphemeralRefName::mint(&project, &WorkweaveName::new("seat").expect("name")),
            ResolvedRevisionId::from_canonical("a".repeat(40), None),
        )
        .expect("the index write succeeds");
    stamp_owned_digest(&project_dir, "Cargo.lock", b"version = 4\n")
        .expect("the ledger stamp succeeds");

    assert_eq!(
        std::fs::metadata(&exclude).expect("surface").ino(),
        identity,
        "the surface was republished as a new file: a writer that replaces it \
         from a read taken earlier drops whatever a peer appended since"
    );
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
