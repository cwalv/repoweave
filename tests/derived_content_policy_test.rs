//! A stated derived-content policy, exercised through a real replay.
//!
//! `regenerable-regions.md` D1–D3: a repo declares which of its paths are
//! derived, in tracked metadata that travels with a clone; rwv supplies the
//! resolution that declaration names, per operation, as a typed parameter.
//! These tests hold both halves to their claims — that the declaration alone
//! does nothing, that the policy is what resolves it, and that the resolution
//! is mechanical.
//!
//! The subject is a *generated document*, not `rwv.lock`. The lock is the
//! primitive's first instance and has its own suite (`vcs_test.rs`,
//! `e2e_sync_lock_replay_test.rs`) which is unchanged by this; using a
//! different derived path here is what shows the primitive is general and
//! keeps the two from being read as one test of the same thing.

use repoweave::git::{has_rwv_merge_driver_config, GitVcs};
use repoweave::vcs::{ConflictOp, DerivedContentPolicy, ResolvedRevisionId, Vcs, VcsError};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

mod common;

/// The derived path under test: a document regenerated from sources, whose
/// committed copy is only ever as fresh as the last generator run.
const DERIVED: &str = "docs/generated/reference.md";

/// An authored path, declared nothing. Its conflicts are genuine and no
/// policy may resolve them.
const AUTHORED: &str = "src/handwritten.md";

fn git(dir: &Path, args: &[&str]) -> String {
    let output = common::git()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run git");
    if !output.status.success() {
        panic!(
            "git {:?} failed in {}: {}",
            args,
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn write(dir: &Path, rel: &str, contents: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn read(dir: &Path, rel: &str) -> String {
    fs::read_to_string(dir.join(rel)).unwrap()
}

/// A repo whose committed `.gitattributes` declares [`DERIVED`] derived, and
/// which defines no resolver for it anywhere.
///
/// The second half is the precondition that gives every assertion below its
/// meaning: with a durable `merge.rwv-ours.*` plant in the repo's config, a
/// declared path would resolve mechanically no matter what any operation
/// passed, and a policy parameter that did nothing would still look right.
fn repo_declaring_derived_content() -> TempDir {
    let dir = common::tempdir().unwrap();
    let p = dir.path();

    git(p, &["init"]);
    git(p, &["config", "user.email", "test@test.com"]);
    git(p, &["config", "user.name", "Test"]);

    write(p, DERIVED, "generated: v0\n");
    write(p, AUTHORED, "authored: v0\n");
    GitVcs
        .set_replay_exclusion(p, Path::new(DERIVED))
        .expect("declaring a path derived writes the repo's tracked metadata");
    git(p, &["add", "."]);
    git(
        p,
        &["commit", "-m", "initial: sources + derived declaration"],
    );

    assert!(
        !has_rwv_merge_driver_config(p).unwrap(),
        "fixture precondition: no resolver may be defined durably in {}, \
         or these tests cannot tell a working policy from an ignored one \
         (is `merge.rwv-ours.driver` set in your global git config?)",
        p.display()
    );

    dir
}

/// Diverge `main` and `feat` on `path`, both from the initial commit.
/// Returns main's tip, which is both the rebase target and the version a
/// target-side resolution must leave standing.
fn diverge_on(p: &Path, path: &str) -> ResolvedRevisionId {
    let base = git(p, &["rev-parse", "HEAD"]);

    write(p, path, "main's version\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-m", "main: regenerate"]);

    git(p, &["checkout", "-b", "feat", &base]);
    write(p, path, "feat's version\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-m", "feat: regenerate"]);

    ResolvedRevisionId::from_canonical(git(p, &["rev-parse", "main"]), None)
}

// ============================================================================
// The policy is what resolves a declared path
// ============================================================================

#[test]
fn keeping_the_target_side_resolves_a_declared_path_without_stopping() {
    let dir = repo_declaring_derived_content();
    let p = dir.path();
    let main_tip = diverge_on(p, DERIVED);

    GitVcs
        .rebase(
            p,
            &main_tip,
            &main_tip,
            DerivedContentPolicy::keep_target_side(),
        )
        .expect("a declared derived path must not stop a replay under this policy");

    assert_eq!(
        read(p, DERIVED),
        "main's version\n",
        "the target side's version must be the one left standing"
    );
    assert!(
        GitVcs::mid_op_state(p).is_none(),
        "nothing may be left in flight: the resolution is mechanical, not something to finish"
    );
    // The replayed commit had nothing left to record once its only change
    // was resolved away, so it is gone rather than sitting empty on top.
    let log = git(p, &["log", "--oneline", "--no-decorate"]);
    assert!(
        !log.contains("feat: regenerate"),
        "a commit that only touched derived content must drop; got log:\n{log}"
    );
}

#[test]
fn the_same_replay_without_the_policy_stops_on_the_same_path() {
    let dir = repo_declaring_derived_content();
    let p = dir.path();
    let main_tip = diverge_on(p, DERIVED);

    // Same fixture, same declaration, same operation — only the policy
    // differs. This is what makes the parameter load-bearing rather than
    // decorative: the declaration on its own resolves nothing.
    let err = GitVcs
        .rebase(p, &main_tip, &main_tip, DerivedContentPolicy::vcs_default())
        .expect_err("with no resolver supplied, a declared path conflicts like any other");

    assert!(
        matches!(err, VcsError::RebaseConflict { ref op, .. } if *op == ConflictOp::Rebase),
        "expected the ordinary textual-merge conflict, got {err:?}"
    );
    assert_eq!(
        GitVcs::mid_op_state(p).as_deref(),
        Some("mid-rebase"),
        "an unresolved conflict must leave the repo where the operator can act on it"
    );
    GitVcs.cancel_in_flight_op(p);
}

#[test]
fn no_policy_resolves_a_path_the_repo_never_declared() {
    let dir = repo_declaring_derived_content();
    let p = dir.path();
    let main_tip = diverge_on(p, AUTHORED);

    // The resolution applies to what the repo declared and nothing else.
    // A policy that swallowed authored conflicts would be losing work.
    let err = GitVcs
        .rebase(
            p,
            &main_tip,
            &main_tip,
            DerivedContentPolicy::keep_target_side(),
        )
        .expect_err("an authored conflict must survive any derived-content policy");

    assert!(
        matches!(err, VcsError::RebaseConflict { ref op, .. } if *op == ConflictOp::Rebase),
        "expected RebaseConflict on the authored path, got {err:?}"
    );
    GitVcs.cancel_in_flight_op(p);
}

// ============================================================================
// The resumed replay carries its own policy
// ============================================================================

/// Diverge with an authored conflict *first* and a declared-path conflict
/// *behind it*, so the second one is only ever reached by a resumed replay.
fn diverge_with_a_conflict_ahead_of_the_declared_one(p: &Path) -> ResolvedRevisionId {
    let base = git(p, &["rev-parse", "HEAD"]);

    write(p, AUTHORED, "main's prose\n");
    write(p, DERIVED, "main's version\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-m", "main: edit prose, regenerate"]);

    git(p, &["checkout", "-b", "feat", &base]);
    write(p, AUTHORED, "feat's prose\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-m", "feat: edit prose"]);
    write(p, DERIVED, "feat's version\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-m", "feat: regenerate"]);

    ResolvedRevisionId::from_canonical(git(p, &["rev-parse", "main"]), None)
}

/// Stop the replay on the authored conflict and resolve it the way an
/// operator would, leaving the declared-path pick still to come.
fn replay_until_the_authored_conflict(p: &Path, main_tip: &ResolvedRevisionId) {
    let err = GitVcs
        .rebase(
            p,
            main_tip,
            main_tip,
            DerivedContentPolicy::keep_target_side(),
        )
        .expect_err("the authored conflict must stop the replay");
    assert!(
        matches!(err, VcsError::RebaseConflict { .. }),
        "expected the replay to stop on the authored path, got {err:?}"
    );

    write(p, AUTHORED, "resolved prose\n");
    git(p, &["add", AUTHORED]);
}

#[test]
fn a_resumed_replay_resolves_the_declared_path_it_reaches() {
    let dir = repo_declaring_derived_content();
    let p = dir.path();
    let main_tip = diverge_with_a_conflict_ahead_of_the_declared_one(p);
    replay_until_the_authored_conflict(p, &main_tip);

    GitVcs
        .rebase_continue(p, DerivedContentPolicy::keep_target_side())
        .expect("the pick behind the resolved conflict must not stop the replay in turn");

    assert_eq!(
        read(p, DERIVED),
        "main's version\n",
        "the target side's version must be the one left standing"
    );
    assert_eq!(
        read(p, AUTHORED),
        "resolved prose\n",
        "the operator's own resolution must be what lands"
    );
    assert!(
        GitVcs::mid_op_state(p).is_none(),
        "the replay must have run to completion"
    );
}

#[test]
fn a_resumed_replay_without_the_policy_stops_on_the_declared_path() {
    let dir = repo_declaring_derived_content();
    let p = dir.path();
    let main_tip = diverge_with_a_conflict_ahead_of_the_declared_one(p);
    replay_until_the_authored_conflict(p, &main_tip);

    // The resume is a fresh operation and states its own policy. Supplying
    // none leaves the picks it reaches to resolve textually — including the
    // declared one the interrupted replay never got to.
    let err = GitVcs
        .rebase_continue(p, DerivedContentPolicy::vcs_default())
        .expect_err("with no resolver supplied, the declared path conflicts in its turn");

    assert!(
        matches!(err, VcsError::RebaseConflict { ref op, .. } if *op == ConflictOp::Rebase),
        "expected RebaseConflict on the declared path, got {err:?}"
    );
    let conflicted = git(p, &["diff", "--name-only", "--diff-filter=U"]);
    assert!(
        conflicted.contains(DERIVED),
        "the declared path must be the one left conflicted; got {conflicted:?}"
    );
    GitVcs.cancel_in_flight_op(p);
}
