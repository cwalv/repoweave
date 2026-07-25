//! The branch model's observation and ref-type surface
//! (`docs/repoweave/branch-model.md` §3.6, §4.2, §4.3, §4.5, §8.8).
//!
//! What these tests are for, stated once so the shape is not mistaken for
//! ceremony:
//!
//!   1. **"Which ref is this checkout on."** The suite had no assertion of
//!      that shape at all — the fetch-detach test asserts `rev-parse HEAD`
//!      equality against a fixture that pre-detached the repo, so it cannot
//!      see a detach. `head_attachment` gives that question a type, and
//!      every test here that touches ref state asserts against it.
//!   2. **Four states, not one `None`.** `current_ref` collapsed "on a
//!      branch" / "unborn" / "detached" / "not a repo at all" into a single
//!      `Ok(None)`. Each is pinned separately below, including the two that
//!      are errors rather than states.
//!   3. **Adversarial, not happy-path.** A witness whose repo moved under
//!      it, a MOVE onto a repo stopped mid-bisect, a create that finds the
//!      branch already there, a remote whose HEAD is unset or malformed.
//!      Those are the cases the shipped code got wrong.
//!
//! Type-level invariants (no cross-type comparison, no `as_str()`
//! laundering, no forged witness) are enforced by `compile_fail` doctests
//! on the types and pinned by error code in
//! `tests/branch_model_compile_fail_test.rs`.

use repoweave::git::GitVcs;
use repoweave::manifest::{ProjectName, Role, WorkweaveName};
use repoweave::vcs::{
    EphemeralRefName, HeadAttachment, RawRefName, RefNameError, ResolvedRevisionId, TrackingRef,
    Vcs, VcsError,
};
use std::path::Path;
use tempfile::TempDir;

mod common;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Run git in `dir`, panicking on failure.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = common::git()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run git");
    assert!(
        output.status.success(),
        "git {:?} failed in {}: {}",
        args,
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// Run git in `dir`, returning whether it succeeded. For the cases where
/// the failure IS the fixture.
fn git_ok(dir: &Path, args: &[&str]) -> bool {
    common::git()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run git")
        .status
        .success()
}

/// A repo with no commits: HEAD is symbolic and points at an unborn branch.
fn empty_repo() -> TempDir {
    let dir = TempDir::new().unwrap();
    git(dir.path(), &["init"]);
    git(dir.path(), &["config", "user.email", "test@test.com"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    dir
}

/// A repo with one commit on `main`.
fn repo_with_commit() -> TempDir {
    let dir = empty_repo();
    commit(dir.path(), "one", "1");
    dir
}

/// Commit `content` to `file` in `repo` and return the resulting tip.
fn commit(repo: &Path, file: &str, content: &str) -> ResolvedRevisionId {
    std::fs::write(repo.join(file), content).unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-m", file]);
    GitVcs.head_revision(repo).unwrap()
}

/// The branch HEAD is on, as git itself reports it — the independent
/// oracle the `head_attachment` assertions are checked against.
fn git_says_branch(repo: &Path) -> Option<String> {
    let out = common::git()
        .args(["symbolic-ref", "--short", "HEAD"])
        .current_dir(repo)
        .output()
        .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8(out.stdout).unwrap().trim().to_owned())
}

// ---------------------------------------------------------------------------
// §4.5 — "no current branch" is not one state
// ---------------------------------------------------------------------------

#[test]
fn head_attachment_reports_the_branch_a_checkout_is_on() {
    let repo = repo_with_commit();

    let observed = GitVcs.head_attachment(repo.path()).unwrap();

    match &observed {
        HeadAttachment::Attached(a) => {
            assert_eq!(a.to_string(), "main");
            assert_eq!(a.repo(), repo.path(), "witness carries its provenance");
        }
        other => panic!("expected Attached, got {other:?}"),
    }
    assert_eq!(
        observed.to_string(),
        "on branch 'main'",
        "the rendering is the operator-facing half of the assertion shape"
    );
    assert_eq!(git_says_branch(repo.path()).as_deref(), Some("main"));
}

#[test]
fn head_attachment_reports_unborn_separately_from_attached() {
    let repo = empty_repo();

    match GitVcs.head_attachment(repo.path()).unwrap() {
        HeadAttachment::Unborn(u) => {
            // `symbolic-ref` succeeds here, so the state is reportable
            // rather than merely diagnosable.
            assert_eq!(u.name().as_str(), "main");
            assert_eq!(u.repo(), repo.path());
        }
        other => panic!("expected Unborn, got {other:?}"),
    }
}

#[test]
fn head_attachment_reports_detached_separately_from_unborn() {
    let repo = repo_with_commit();
    let tip = GitVcs.head_revision(repo.path()).unwrap();
    git(repo.path(), &["checkout", "--detach", tip.as_str()]);

    match GitVcs.head_attachment(repo.path()).unwrap() {
        HeadAttachment::Detached(d) => {
            assert_eq!(d.at(), &tip);
            assert_eq!(d.repo(), repo.path());
        }
        other => panic!("expected Detached, got {other:?}"),
    }
    assert_eq!(
        git_says_branch(repo.path()),
        None,
        "fixture really is detached"
    );
}

#[test]
fn head_attachment_on_a_non_repo_is_not_a_detached_head() {
    // The shipped `current_ref` returned `Ok(None)` here, which is why
    // `rwv push` reported "is on a detached HEAD" for a directory with no
    // git in it at all.
    let dir = TempDir::new().unwrap();

    match GitVcs.head_attachment(dir.path()) {
        Err(VcsError::NotARepo(p)) => assert_eq!(p, dir.path()),
        other => panic!("expected NotARepo, got {other:?}"),
    }
}

#[test]
fn head_attachment_kinds_are_distinguishable_in_json_output() {
    // Doctor / push / lock all report through the `--json` surface; the
    // point of the split is lost if the wire form re-collapses it.
    let dir = TempDir::new().unwrap();
    let err = GitVcs.head_attachment(dir.path()).unwrap_err();
    assert_eq!(err.kind(), "not-a-repo");
}

// ---------------------------------------------------------------------------
// §4.3 — MOVE takes a witness, and the witness carries its repo
// ---------------------------------------------------------------------------

#[test]
fn advance_attached_ref_moves_the_ref_the_checkout_is_on() {
    let repo = repo_with_commit();
    let base = GitVcs.head_revision(repo.path()).unwrap();

    // Build a target commit on a side branch, then come back to main.
    git(repo.path(), &["checkout", "-b", "side"]);
    let target = commit(repo.path(), "two", "2");
    git(repo.path(), &["checkout", "main"]);

    let HeadAttachment::Attached(witness) = GitVcs.head_attachment(repo.path()).unwrap() else {
        panic!("fixture should be attached");
    };
    GitVcs.advance_attached_ref(&witness, &target).unwrap();

    // Which ref is this checkout on? Still `main` — a MOVE does not change
    // the attachment, which is the whole of R1.
    let after = GitVcs.head_attachment(repo.path()).unwrap();
    assert_eq!(after.to_string(), "on branch 'main'");
    assert_eq!(GitVcs.head_revision(repo.path()).unwrap(), target);
    assert_ne!(target, base);
}

#[test]
fn advance_attached_ref_refuses_a_witness_whose_repo_moved_under_it() {
    let repo = repo_with_commit();
    git(repo.path(), &["checkout", "-b", "side"]);
    let target = commit(repo.path(), "two", "2");
    git(repo.path(), &["checkout", "main"]);

    let HeadAttachment::Attached(witness) = GitVcs.head_attachment(repo.path()).unwrap() else {
        panic!("fixture should be attached");
    };

    // Something else moves the repo between observation and consumption —
    // the TOCTOU form of the cross-repo pass.
    git(repo.path(), &["checkout", "side"]);

    match GitVcs.advance_attached_ref(&witness, &target) {
        Err(VcsError::StaleRefWitness {
            repo: r,
            expected,
            observed,
        }) => {
            assert_eq!(r, repo.path());
            assert_eq!(expected, "on branch 'main'");
            assert_eq!(observed, "on branch 'side'");
        }
        other => panic!("expected StaleRefWitness, got {other:?}"),
    }
}

#[test]
fn advance_attached_ref_refuses_a_witness_for_a_repo_that_became_detached() {
    let repo = repo_with_commit();
    let base = GitVcs.head_revision(repo.path()).unwrap();
    git(repo.path(), &["checkout", "-b", "side"]);
    let target = commit(repo.path(), "two", "2");
    git(repo.path(), &["checkout", "main"]);

    let HeadAttachment::Attached(witness) = GitVcs.head_attachment(repo.path()).unwrap() else {
        panic!("fixture should be attached");
    };
    git(repo.path(), &["checkout", "--detach", base.as_str()]);

    // This is the shape that landed commits on nothing: a phase detaches a
    // repo while a later phase still holds a witness for it.
    let err = GitVcs.advance_attached_ref(&witness, &target).unwrap_err();
    assert_eq!(err.kind(), "stale-ref-witness");
    assert_eq!(
        GitVcs.head_revision(repo.path()).unwrap(),
        base,
        "the refusal is a refusal: nothing moved"
    );
}

#[test]
fn a_witness_from_one_repo_cannot_move_another() {
    // The cross-repo pass has no runtime form to test: the witness carries
    // its repo and the MOVE takes no independent path, so the only way to
    // name a second repo is a witness for THAT repo. What is testable is
    // that the target really is derived from the witness.
    let repo_a = repo_with_commit();
    let repo_b = repo_with_commit();
    git(repo_b.path(), &["checkout", "-b", "side"]);
    let b_target = commit(repo_b.path(), "two", "2");
    git(repo_b.path(), &["checkout", "main"]);

    let HeadAttachment::Attached(witness_a) = GitVcs.head_attachment(repo_a.path()).unwrap() else {
        panic!("fixture should be attached");
    };

    // `b_target` is not an object in repo A at all, so the MOVE derived from
    // witness A fails against A rather than quietly advancing B.
    assert!(GitVcs.advance_attached_ref(&witness_a, &b_target).is_err());
    assert_eq!(
        GitVcs.head_attachment(repo_b.path()).unwrap().to_string(),
        "on branch 'main'"
    );
    assert_ne!(GitVcs.head_revision(repo_b.path()).unwrap(), b_target);
}

#[test]
fn advance_attached_ref_refuses_a_non_fast_forward() {
    let repo = repo_with_commit();
    let base = GitVcs.head_revision(repo.path()).unwrap();
    git(repo.path(), &["checkout", "-b", "side"]);
    git(repo.path(), &["reset", "--hard", base.as_str()]);
    let divergent = commit(repo.path(), "side-only", "s");
    git(repo.path(), &["checkout", "main"]);
    let _ = commit(repo.path(), "main-only", "m");
    let main_tip = GitVcs.head_revision(repo.path()).unwrap();

    let HeadAttachment::Attached(witness) = GitVcs.head_attachment(repo.path()).unwrap() else {
        panic!("fixture should be attached");
    };
    assert!(GitVcs.advance_attached_ref(&witness, &divergent).is_err());
    assert_eq!(GitVcs.head_revision(repo.path()).unwrap(), main_tip);
}

// ---------------------------------------------------------------------------
// §3.6 — the mid-operation precondition on detached MOVEs
// ---------------------------------------------------------------------------

#[test]
fn advance_detached_head_moves_a_detached_head() {
    let repo = repo_with_commit();
    let base = GitVcs.head_revision(repo.path()).unwrap();
    let target = commit(repo.path(), "two", "2");
    git(repo.path(), &["checkout", "--detach", base.as_str()]);

    let HeadAttachment::Detached(witness) = GitVcs.head_attachment(repo.path()).unwrap() else {
        panic!("fixture should be detached");
    };
    GitVcs.advance_detached_head(&witness, &target).unwrap();

    // Still detached: moving a detached HEAD is a MOVE, not an ATTACH.
    match GitVcs.head_attachment(repo.path()).unwrap() {
        HeadAttachment::Detached(d) => assert_eq!(d.at(), &target),
        other => panic!("expected Detached, got {other:?}"),
    }
}

#[test]
fn advance_detached_head_refuses_a_repo_stopped_mid_bisect() {
    // `Detached` collapses "rwv detached this at a lock SHA" with "the
    // operator is mid-bisect". Only the first is rwv's to move, and the
    // shipped detection did not look for a bisect at all.
    let repo = repo_with_commit();
    let base = GitVcs.head_revision(repo.path()).unwrap();
    let mid = commit(repo.path(), "two", "2");
    let tip = commit(repo.path(), "three", "3");

    git(repo.path(), &["bisect", "start"]);
    git(repo.path(), &["bisect", "bad", tip.as_str()]);
    git(repo.path(), &["bisect", "good", base.as_str()]);

    let bisect_position = GitVcs.head_revision(repo.path()).unwrap();
    let HeadAttachment::Detached(witness) = GitVcs.head_attachment(repo.path()).unwrap() else {
        panic!("a bisect leaves HEAD detached");
    };

    match GitVcs.advance_detached_head(&witness, &mid) {
        Err(VcsError::MidOperation { repo: r, operation }) => {
            assert_eq!(r, repo.path());
            assert_eq!(operation, "mid-bisect", "the refusal names the operation");
        }
        other => panic!("expected MidOperation, got {other:?}"),
    }
    assert_eq!(
        GitVcs.head_revision(repo.path()).unwrap(),
        bisect_position,
        "the operator's bisect position survives the refusal"
    );
}

#[test]
fn mid_operation_sees_a_bisect_that_mid_op_cannot() {
    let repo = repo_with_commit();
    let base = GitVcs.head_revision(repo.path()).unwrap();
    let _ = commit(repo.path(), "two", "2");
    let tip = commit(repo.path(), "three", "3");
    git(repo.path(), &["bisect", "start"]);
    git(repo.path(), &["bisect", "bad", tip.as_str()]);
    git(repo.path(), &["bisect", "good", base.as_str()]);

    assert_eq!(
        GitVcs.mid_operation(repo.path()).as_deref(),
        Some("mid-bisect")
    );
    assert!(
        GitVcs.mid_op(repo.path()).is_none(),
        "a bisect has no conflict-resume path, so it is not a ConflictOp — \
         which is exactly why the precondition needs its own accessor"
    );
}

#[test]
fn mid_operation_is_none_in_a_clean_repo() {
    let repo = repo_with_commit();
    assert_eq!(GitVcs.mid_operation(repo.path()), None);
}

#[test]
fn advance_detached_head_refuses_a_stale_witness() {
    let repo = repo_with_commit();
    let base = GitVcs.head_revision(repo.path()).unwrap();
    let target = commit(repo.path(), "two", "2");
    git(repo.path(), &["checkout", "--detach", base.as_str()]);

    let HeadAttachment::Detached(witness) = GitVcs.head_attachment(repo.path()).unwrap() else {
        panic!("fixture should be detached");
    };
    // Someone reattaches between observation and consumption.
    git(repo.path(), &["checkout", "main"]);

    let err = GitVcs.advance_detached_head(&witness, &target).unwrap_err();
    assert_eq!(err.kind(), "stale-ref-witness");
    assert_eq!(
        GitVcs.head_attachment(repo.path()).unwrap().to_string(),
        "on branch 'main'"
    );
}

// ---------------------------------------------------------------------------
// §4.2 — TrackingRef::parse and §8.8's rejections
// ---------------------------------------------------------------------------

#[test]
fn tracking_ref_accepts_a_branch_name() {
    let t = TrackingRef::parse(RawRefName::new("main")).unwrap();
    assert_eq!(t.to_string(), "main");
    assert_eq!(t.local_counterpart().as_str(), "main");

    let nested = TrackingRef::parse(RawRefName::new("release/1.x")).unwrap();
    assert_eq!(nested.to_string(), "release/1.x");
}

#[test]
fn tracking_ref_rejects_commit_id_shaped_input() {
    // `version:` declares what to TRACK; the lock records where you ARE.
    for pin in [
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "deadbeef",
        "0123456",
    ] {
        match TrackingRef::parse(RawRefName::new(pin)) {
            Err(RefNameError::ShaShaped(s)) => assert_eq!(s, pin),
            other => panic!("expected ShaShaped for {pin:?}, got {other:?}"),
        }
    }
}

#[test]
fn tracking_ref_accepts_short_hex_words_that_are_plausible_branch_names() {
    // The rejection is a shape heuristic with a stated floor, not "contains
    // hex digits". Below git's abbreviation floor these are just words.
    for word in ["cafe", "beef", "dad"] {
        assert!(
            TrackingRef::parse(RawRefName::new(word)).is_ok(),
            "{word} should parse"
        );
    }
}

#[test]
fn tracking_ref_rejects_tag_shaped_input() {
    for tag in ["v1.0", "v1.2.3", "v0.3.4-rc1"] {
        match TrackingRef::parse(RawRefName::new(tag)) {
            Err(RefNameError::TagShaped(s)) => assert_eq!(s, tag),
            other => panic!("expected TagShaped for {tag:?}, got {other:?}"),
        }
    }
}

#[test]
fn tracking_ref_rejects_names_git_itself_would_refuse() {
    for (name, why) in [
        ("", "empty"),
        ("feat/../etc", "dot dot"),
        ("has space", "space"),
        ("caret^", "caret"),
        ("tilde~1", "tilde"),
        ("colon:name", "colon"),
        ("star*", "glob"),
        ("trailing/", "trailing slash"),
        ("/leading", "leading slash"),
        ("double//slash", "empty component"),
        ("ends.", "trailing dot"),
        (".hidden", "dot-leading component"),
        ("feat/.hidden", "dot-leading component"),
        ("branch.lock", "reflog-name collision"),
        ("@", "bare @"),
        ("at@{0}", "reflog syntax"),
    ] {
        assert!(
            TrackingRef::parse(RawRefName::new(name)).is_err(),
            "{name:?} should be rejected ({why})"
        );
    }
}

#[test]
fn tracking_ref_projections_are_named_not_implicit() {
    let t = TrackingRef::parse(RawRefName::new("main")).unwrap();

    // Two different questions, two different projections. Neither is an
    // identity, and each has a function whose doc states the assumption.
    assert_eq!(t.local_counterpart().as_str(), "main");
    let remote = t.on_remote(Role::Owned);
    assert_eq!(remote.branch(), "main");
    assert_eq!(remote.role(), Role::Owned);
    assert_eq!(remote.to_string(), "main on the owned remote");
}

// ---------------------------------------------------------------------------
// §3.5 / §4.2 — EphemeralRefName::mint
// ---------------------------------------------------------------------------

#[test]
fn ephemeral_names_are_flat_and_derived_from_exactly_two_inputs() {
    let name = EphemeralRefName::mint(
        &ProjectName::new("foundations"),
        &WorkweaveName::new("fix-42"),
    );
    assert_eq!(name.to_string(), "foundations--fix-42");
    assert_eq!(name.to_raw().as_str(), "foundations--fix-42");
    assert!(
        !name.to_string().contains('/'),
        "no third component: nothing read it, and three sites disagreed \
         about what it should be"
    );
}

#[test]
fn ephemeral_name_minting_is_total() {
    // Deliberately no validation: the legal grammar for project and
    // workweave names is an open question, and under ownership-by-receipt a
    // collision is a legibility problem rather than a correctness one.
    let odd = EphemeralRefName::mint(&ProjectName::new("p--x"), &WorkweaveName::new("y"));
    assert_eq!(odd.to_string(), "p--x--y");
}

// ---------------------------------------------------------------------------
// §4.2 — RemoteDefaultBranch: the absence is explicit
// ---------------------------------------------------------------------------

#[test]
fn remote_default_branch_is_none_when_origin_head_is_unset() {
    // The shipped implementation fabricated "main" here, so the publish
    // gate compared an observation against an invention.
    let repo = repo_with_commit();
    assert!(GitVcs.remote_default_branch(repo.path()).unwrap().is_none());
}

#[test]
fn remote_default_branch_reads_the_symref_when_it_is_set() {
    let repo = repo_with_commit();
    git(repo.path(), &["branch", "trunk"]);
    git(
        repo.path(),
        &[
            "update-ref",
            "refs/remotes/origin/trunk",
            "refs/heads/trunk",
        ],
    );
    git(
        repo.path(),
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ],
    );

    let observed = GitVcs.remote_default_branch(repo.path()).unwrap();
    let observed = observed.expect("symref is set");
    assert_eq!(observed.to_string(), "trunk");
    assert_eq!(
        observed.local_counterpart().as_str(),
        "trunk",
        "the publish gate compares through a named projection, not a cast"
    );
}

#[test]
fn remote_default_branch_is_none_for_a_symref_outside_the_remote_namespace() {
    // A malformed symref is an absence, never a default. This is reachable
    // for real: `git remote set-head` is not the only way `origin/HEAD` gets
    // written, and a hand-written one can point anywhere.
    let repo = repo_with_commit();
    git(
        repo.path(),
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/heads/main",
        ],
    );

    assert!(
        GitVcs.remote_default_branch(repo.path()).unwrap().is_none(),
        "a symref pointing outside the remote namespace names no default — \
         and there is no fallback to invent one"
    );
    // The rule's exhaustive cases (empty target, empty branch, malformed
    // branch) are unit-tested against `from_symref_target` in `src/vcs.rs`,
    // where the rule lives.
}

#[test]
fn remote_default_branch_on_a_non_repo_is_an_error_not_an_absence() {
    let dir = TempDir::new().unwrap();
    match GitVcs.remote_default_branch(dir.path()) {
        Err(VcsError::NotARepo(p)) => assert_eq!(p, dir.path()),
        other => panic!("expected NotARepo, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// §4.3 — listings are report-only by type
// ---------------------------------------------------------------------------

#[test]
fn listings_return_observed_names() {
    let repo = repo_with_commit();
    git(repo.path(), &["branch", "p--ww"]);
    git(repo.path(), &["branch", "p--other"]);
    git(repo.path(), &["branch", "unrelated"]);

    let mut all: Vec<String> = GitVcs
        .list_local_branch_names(repo.path())
        .unwrap()
        .iter()
        .map(|n| n.as_str().to_owned())
        .collect();
    all.sort();
    assert_eq!(all, ["main", "p--other", "p--ww", "unrelated"]);

    let mut prefixed: Vec<String> = GitVcs
        .list_branch_names_with_prefix(repo.path(), "p--")
        .unwrap()
        .iter()
        .map(|n| n.as_str().to_owned())
        .collect();
    prefixed.sort();
    assert_eq!(
        prefixed,
        ["p--other", "p--ww"],
        "flat names match a flat prefix; the shipped glob required a slash \
         and so matched none of them"
    );
}

#[test]
fn listing_a_prefix_with_no_matches_is_empty_not_an_error() {
    let repo = repo_with_commit();
    assert!(GitVcs
        .list_branch_names_with_prefix(repo.path(), "nothing--")
        .unwrap()
        .is_empty());
}

// ---------------------------------------------------------------------------
// The canonical-form check that replaced the unchecked constructor
// ---------------------------------------------------------------------------

#[test]
fn resolved_revisions_from_ref_resolution_are_canonical_by_construction() {
    let sha1 = "a".repeat(40);
    let sha256 = "b".repeat(64);
    assert_eq!(
        ResolvedRevisionId::from_rev_parse_output(&sha1).map(|r| r.as_str().to_owned()),
        Some(sha1.clone())
    );
    assert!(ResolvedRevisionId::from_rev_parse_output(&sha256).is_some());
    // Trailing newline from command output is tolerated; everything else
    // that is not a canonical object name is refused rather than asserted
    // away by a doc comment.
    assert!(ResolvedRevisionId::from_rev_parse_output(&format!("{sha1}\n")).is_some());
    for bad in [
        "",
        "abc1234",
        "not-a-sha",
        "refs/rwv/pre-op/op-1",
        &"A".repeat(40),
        &"a".repeat(39),
        &"a".repeat(41),
        &"g".repeat(40),
    ] {
        assert!(
            ResolvedRevisionId::from_rev_parse_output(bad).is_none(),
            "{bad:?} is not a canonical commit id"
        );
    }
}

#[test]
fn savepoint_resolution_still_round_trips_through_the_checked_constructor() {
    let repo = repo_with_commit();
    let head = GitVcs.head_revision(repo.path()).unwrap();

    let captured = GitVcs.create_savepoint(repo.path(), "op-1").unwrap();
    assert_eq!(captured, head);
    assert_eq!(GitVcs.resolve_savepoint(repo.path(), "op-1"), Some(head));
    assert_eq!(GitVcs.resolve_savepoint(repo.path(), "absent"), None);
}

#[test]
fn a_savepoint_ref_is_proof_the_savepoint_exists() {
    let repo = repo_with_commit();
    let head = GitVcs.head_revision(repo.path()).unwrap();

    let savepoint = GitVcs.create_savepoint_ref(repo.path(), "op-2").unwrap();
    assert_eq!(savepoint.repo(), repo.path());
    assert_eq!(savepoint.op_id(), "op-2");
    assert_eq!(savepoint.at(), &head);
    assert!(
        git_ok(
            repo.path(),
            &["rev-parse", "--verify", "refs/rwv/pre-op/op-2"]
        ),
        "the ref is on disk, not merely planned"
    );
}
