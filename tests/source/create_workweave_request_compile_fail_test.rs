//! `create_workweave` takes a `CreateWorkweave` whose two roots are named
//! where they are given: the primary weave and the workspace forked from are
//! both `&Path`, so a positional call that swapped them compiled. Pinned by
//! diagnostic code, from an external crate.
//!
//! Residue: a call can still name the wrong value for a root
//! (`primary_root: source`); it says so in the text a reviewer reads.

use crate::common;

use common::compile_probe::{assert_fails_with, compile};

#[test]
fn the_harness_can_create_with_named_roots() {
    // Control: the request the probe below spells positionally.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::activate::ActivateOptions;
        use repoweave::manifest::{ProjectName, WorkweaveName};
        use repoweave::workweave::{create_workweave, CreateWorkweave};
        use std::path::Path;
        pub fn create(primary: &Path, source: &Path, p: &ProjectName, n: &WorkweaveName) {
            let _ = create_workweave(CreateWorkweave {
                primary_root: primary,
                source_root: source,
                project: p,
                name: n,
                branch: repoweave::workweave::BranchChoice::Minted,
                spelling: None,
                at: None,
                entries: Vec::new(),
                placement: &repoweave::workweave::Placement::DefaultBasedir,
                activation: ActivateOptions::default(),
            });
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn the_roots_cannot_be_passed_by_position() {
    assert_fails_with(
        "E0061",
        "a positional call is where the two roots could swap unseen",
        r#"
        use repoweave::activate::ActivateOptions;
        use repoweave::manifest::{ProjectName, WorkweaveName};
        use repoweave::workweave::{create_workweave, BranchChoice};
        use std::path::Path;
        pub fn create(primary: &Path, source: &Path, p: &ProjectName, n: &WorkweaveName) {
            let _ = create_workweave(
                source,
                primary,
                p,
                n,
                BranchChoice::Minted,
                None,
                None,
                Vec::new(),
                &repoweave::workweave::Placement::DefaultBasedir,
                ActivateOptions::default(),
            );
        }
        "#,
    );
}
