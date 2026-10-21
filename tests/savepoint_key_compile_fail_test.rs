//! A savepoint is filed under a `SavepointKey`, never under text, and no op id
//! can hold the target-side namespace.
//!
//! The target side of a `sync-to` files its savepoints and pre-abort references
//! under `<op id>-target`, so that a worktree pair sharing one refdb does not
//! collide. When that name was composed into an `OpId` it was an op id in type
//! only, and every `Vcs` savepoint method took text, so any string reached the
//! ref layout. Each of those methods takes a `SavepointKey`, an `OpId` has no
//! constructor from text that skips validation, and `-target` is a variant of
//! the key rather than a suffix a caller appends. Pinned by diagnostic code,
//! from an external crate, where every visibility narrower than `pub` looks
//! alike.
//!
//! Residue: what an op id may spell, and that the key's two spellings read
//! back as the keys they came from, are runtime facts of `OpId` and
//! `SavepointKey`, pinned beside them in `op_state.rs`.

mod common;

use common::compile_probe::{assert_fails_n_times, assert_fails_with, compile};

#[test]
fn the_harness_can_file_a_savepoint_under_a_key() {
    // Control. Everything below asserts a failure, so a broken rustc
    // invocation would make them all pass for the wrong reason.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::op_state::{OpId, SavepointKey};
        use repoweave::vcs::{ForeignTipPolicy, Vcs};
        use std::path::Path;
        pub fn file(vcs: &dyn Vcs, repo: &Path, op: &OpId) {
            let key = SavepointKey::SyncToTarget(op.clone());
            let _ = vcs.create_savepoint(repo, &key);
            let _ = vcs.savepoint_label(&key);
            let _ = vcs.resolve_savepoint(repo, &key);
            vcs.drop_savepoint(repo, &key);
            let _ = vcs.create_pre_abort_ref(repo, &key, ForeignTipPolicy::Refuse);
            let _ = vcs.resolve_pre_abort_ref(repo, &key);
            let _ = vcs.verified_restore_savepoint(repo, &key, None, None, ForeignTipPolicy::Refuse);
            let _ = vcs.create_savepoint_ref(repo, &key);
            let _ = vcs.resolve_savepoint_ref(repo, &key);
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn no_savepoint_method_takes_text() {
    assert_fails_n_times(
        "E0308",
        9,
        "each of the nine savepoint methods takes a SavepointKey, not a string",
        r#"
        use repoweave::vcs::{ForeignTipPolicy, Vcs};
        use std::path::Path;
        pub fn file(vcs: &dyn Vcs, repo: &Path) {
            let _ = vcs.create_savepoint(repo, "x");
            let _ = vcs.savepoint_label("x");
            let _ = vcs.resolve_savepoint(repo, "x");
            vcs.drop_savepoint(repo, "x");
            let _ = vcs.create_pre_abort_ref(repo, "x", ForeignTipPolicy::Refuse);
            let _ = vcs.resolve_pre_abort_ref(repo, "x");
            let _ = vcs.verified_restore_savepoint(repo, "x", None, None, ForeignTipPolicy::Refuse);
            let _ = vcs.create_savepoint_ref(repo, "x");
            let _ = vcs.resolve_savepoint_ref(repo, "x");
        }
        "#,
    );
}

#[test]
fn an_op_id_has_no_constructor_from_text_that_skips_validation() {
    assert_fails_with(
        "E0599",
        "OpId::from_string is gone",
        r#"
        use repoweave::op_state::OpId;
        pub fn mint(text: String) -> OpId {
            OpId::from_string(text)
        }
        "#,
    );
}

#[test]
fn an_op_id_cannot_be_built_from_its_field() {
    assert_fails_with(
        "E0423",
        "OpId's field is private to op_state",
        r#"
        use repoweave::op_state::OpId;
        pub fn mint(text: String) -> OpId {
            OpId(text)
        }
        "#,
    );
}

#[test]
fn an_op_id_cannot_be_converted_from_a_string() {
    assert_fails_with(
        "E0277",
        "OpId has no From<String>",
        r#"
        use repoweave::op_state::OpId;
        pub fn mint(text: String) -> OpId {
            text.into()
        }
        "#,
    );
}

#[test]
fn a_target_side_key_does_not_take_text() {
    assert_fails_with(
        "E0308",
        "the target namespace is a variant over an OpId, not a suffix on text",
        r#"
        use repoweave::op_state::SavepointKey;
        pub fn target(text: String) -> SavepointKey {
            SavepointKey::SyncToTarget(format!("{text}-target"))
        }
        "#,
    );
}
