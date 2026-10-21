//! A worktree birth says whether it authored its branch or adopted one that
//! was already there with a `RefOrigin`, never a bool.
//!
//! Rollback deletes a branch only when the birth authored it, so an
//! implementation whose `true` meant the other thing would delete a branch
//! that held someone's commits. Both materialize methods return `RefOrigin`,
//! and the two outcomes are named at every producer and consumer. Pinned by
//! diagnostic code, from an external crate, where every visibility narrower
//! than `pub` looks alike.
//!
//! Residue: this pins the shape of the answer, not which variant a backend
//! returns for which state. That is the behavior tests on `create_worktree_on`
//! (adopts a pre-existing branch; authors a new one).

mod common;

use common::compile_probe::{assert_fails_with, compile};

#[test]
fn the_harness_can_read_a_worktree_birth_as_a_ref_origin() {
    // Control. Everything below asserts a failure, so a broken rustc
    // invocation would make them all pass for the wrong reason.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::vcs::{
            RawRefName, RefOrigin, ResolvedRevisionId, Vcs, VcsError, WorkspacePopulation,
        };
        use std::path::Path;
        pub fn adopted(
            vcs: &dyn Vcs,
            store: &Path,
            dest: &Path,
            name: &RawRefName,
            at: &ResolvedRevisionId,
        ) -> Result<bool, VcsError> {
            Ok(vcs.materialize_worktree_on_ref(store, dest, name, at)? == RefOrigin::Adopted)
        }
        pub fn authored_unpopulated(
            vcs: &dyn WorkspacePopulation,
            store: &Path,
            dest: &Path,
            name: &RawRefName,
            at: &ResolvedRevisionId,
        ) -> Result<bool, VcsError> {
            match vcs.materialize_unpopulated_worktree_on_ref(store, dest, name, at)? {
                RefOrigin::Authored => Ok(true),
                RefOrigin::Adopted => Ok(false),
            }
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn materialize_worktree_on_ref_does_not_answer_with_a_bool() {
    assert_fails_with(
        "E0308",
        "a populated worktree birth answers with a RefOrigin",
        r#"
        use repoweave::vcs::{RawRefName, ResolvedRevisionId, Vcs, VcsError};
        use std::path::Path;
        pub fn authored(
            vcs: &dyn Vcs,
            store: &Path,
            dest: &Path,
            name: &RawRefName,
            at: &ResolvedRevisionId,
        ) -> Result<bool, VcsError> {
            vcs.materialize_worktree_on_ref(store, dest, name, at)
        }
        "#,
    );
}

#[test]
fn materialize_unpopulated_worktree_on_ref_does_not_answer_with_a_bool() {
    assert_fails_with(
        "E0308",
        "an unpopulated worktree birth answers with a RefOrigin",
        r#"
        use repoweave::vcs::{RawRefName, ResolvedRevisionId, VcsError, WorkspacePopulation};
        use std::path::Path;
        pub fn authored(
            vcs: &dyn WorkspacePopulation,
            store: &Path,
            dest: &Path,
            name: &RawRefName,
            at: &ResolvedRevisionId,
        ) -> Result<bool, VcsError> {
            vcs.materialize_unpopulated_worktree_on_ref(store, dest, name, at)
        }
        "#,
    );
}

#[test]
fn an_implementation_cannot_answer_a_worktree_birth_with_a_bool() {
    assert_fails_with(
        "E0053",
        "a backend names whether it authored or adopted, it does not pick a polarity for true",
        r#"
        use repoweave::vcs::{RawRefName, ResolvedRevisionId, VcsError, WorkspacePopulation};
        use std::path::Path;
        pub struct Backend;
        impl WorkspacePopulation for Backend {
            fn materialize_unpopulated_worktree_on_ref(
                &self,
                _store: &Path,
                _dest: &Path,
                _name: &RawRefName,
                _start_point: &ResolvedRevisionId,
            ) -> Result<bool, VcsError> {
                Ok(true)
            }
        }
        "#,
    );
}

#[test]
fn a_ref_origin_is_not_a_bool() {
    assert_fails_with(
        "E0308",
        "RefOrigin does not coerce to a condition",
        r#"
        use repoweave::vcs::RefOrigin;
        pub fn authored(origin: RefOrigin) -> bool {
            if origin {
                return true;
            }
            false
        }
        "#,
    );
}

#[test]
fn a_ref_origin_cannot_be_made_from_a_bool() {
    assert_fails_with(
        "E0277",
        "RefOrigin has no From<bool>",
        r#"
        use repoweave::vcs::RefOrigin;
        pub fn origin(authored: bool) -> RefOrigin {
            authored.into()
        }
        "#,
    );
}
