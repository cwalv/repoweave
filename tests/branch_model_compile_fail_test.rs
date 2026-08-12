//! The branch model's type-level invariants, pinned **by error code**.
//!
//! The types carry `compile_fail` doctests, mirroring the precedent
//! `RawRevisionId` set. Those are the enforcement that survives a refactor
//! — they fail in CI the day someone adds the `PartialEq` or the `From`
//! impl to "make it easier". What they do **not** do is check *why* the
//! snippet failed: on stable, rustdoc accepts the `Exxxx` annotation and
//! ignores it, so a `compile_fail,E0599` doctest passes when the snippet
//! fails with an unrelated E0308 — or with a typo.
//!
//! That gap matters here more than usual. The type split removes `as_str()`
//! from three types specifically so the two shipped comparison lines in
//! `push.rs` stop compiling, and the claim is that they fail with **E0599,
//! no such method** rather than merely "somehow". A doctest cannot make
//! that claim; `common::compile_probe` can. Each case below compiles a
//! snippet against the built library with `rustc` and asserts the exact
//! diagnostic code.
//!
//! The first test is a control that must **succeed**: without it, a broken
//! invocation would make every other assertion pass vacuously.

mod common;

use common::compile_probe::{assert_fails_n_times, assert_fails_with, compile};

#[test]
fn the_harness_can_compile_a_legal_snippet() {
    // Control. Everything below asserts a failure, so a broken rustc
    // invocation would make them all pass for the wrong reason.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::vcs::{RawRefName, TrackingRef};
        pub fn legal() -> String {
            let t = TrackingRef::parse(RawRefName::new("main")).unwrap();
            t.local_counterpart().as_str().to_owned()
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

// ---------------------------------------------------------------------------
// §4.6 (2) — comparing a declared branch with an observed one
// ---------------------------------------------------------------------------

#[test]
fn an_attached_ref_cannot_be_compared_with_a_tracking_ref() {
    // E0308, not E0277: each type derives `PartialEq` against itself, so
    // the right-hand side of `==` is already typed and rustc reports the
    // mismatch before it gets as far as an unsatisfied trait bound. The
    // refusal is the same one either way — "expected `AttachedRef`, found
    // `TrackingRef`" — and pinning the code is how that stays true.
    assert_fails_with(
        "E0308",
        "a declared channel and an observed attachment are different notions",
        r#"
        use repoweave::vcs::{AttachedRef, RawRefName, TrackingRef};
        pub fn compare(attached: AttachedRef) -> bool {
            let declared = TrackingRef::parse(RawRefName::new("main")).unwrap();
            attached == declared
        }
        "#,
    );
}

#[test]
fn an_attached_ref_has_no_as_str() {
    // One type per probe. The two-sided form below emits an error per side,
    // and `AttachedRef::as_str()` alone would re-enable `push.rs:367` —
    // which already holds a single-sided `.as_str()` on the value that
    // becomes an `AttachedRef` — with the suite still green.
    assert_fails_with(
        "E0599",
        "a witness is compared witness-to-witness, never as a string",
        r#"
        use repoweave::vcs::AttachedRef;
        pub fn spell(attached: &AttachedRef) -> String {
            attached.as_str().to_owned()
        }
        "#,
    );
}

#[test]
fn a_tracking_ref_has_no_as_str() {
    // The other half. A declaration must be projected
    // (`local_counterpart` / `on_remote`) before it can be compared, and
    // the projection is the decision the string comparison was hiding.
    assert_fails_with(
        "E0599",
        "a declaration must be projected, not spelled",
        r#"
        use repoweave::vcs::{RawRefName, TrackingRef};
        pub fn spell() -> String {
            let declared = TrackingRef::parse(RawRefName::new("main")).unwrap();
            declared.as_str().to_owned()
        }
        "#,
    );
}

#[test]
fn the_comparison_cannot_be_laundered_through_as_str() {
    // This is the shipped spelling. `push.rs` compares with `.as_str()` on
    // both sides at two sites, so if these types carried `as_str()` the
    // lines would compile verbatim after the split and the whole exercise
    // would report nothing. Both sides must fail: one error left standing
    // means the other type got its accessor back.
    assert_fails_n_times(
        "E0599",
        2,
        "as_str() must not exist on AttachedRef or TrackingRef",
        r#"
        use repoweave::vcs::{AttachedRef, RawRefName, TrackingRef};
        pub fn compare(attached: AttachedRef) -> bool {
            let declared = TrackingRef::parse(RawRefName::new("main")).unwrap();
            attached.as_str() != declared.as_str()
        }
        "#,
    );
}

#[test]
fn an_owned_ref_has_no_as_str_either() {
    assert_fails_with(
        "E0599",
        "a receipt is compared by a named predicate, not by string",
        r#"
        use repoweave::vcs::OwnedRef;
        pub fn spell(owned: &OwnedRef) -> &str {
            owned.as_str()
        }
        "#,
    );
}

#[test]
fn a_remote_default_branch_has_no_as_str() {
    // The other side of the L1 publish gate. The gate compares through
    // `local_counterpart()`, which is where the assumption is stated.
    assert_fails_with(
        "E0599",
        "the publish gate must project, not cast",
        r#"
        use repoweave::vcs::RemoteDefaultBranch;
        pub fn spell(d: &RemoteDefaultBranch) -> &str {
            d.as_str()
        }
        "#,
    );
}

// ---------------------------------------------------------------------------
// §4.2 — a witness cannot be forged
// ---------------------------------------------------------------------------

#[test]
fn an_attached_ref_cannot_be_constructed_by_hand() {
    assert_fails_with(
        "E0451",
        "the only producer of a witness is Vcs::head_attachment",
        r#"
        use repoweave::vcs::{AttachedRef, RawRefName};
        use std::path::PathBuf;
        pub fn forge() -> AttachedRef {
            AttachedRef { repo: PathBuf::from("/tmp/repo"), name: RawRefName::new("main") }
        }
        "#,
    );
}

#[test]
fn a_receipt_cannot_be_minted_outside_the_registry() {
    assert_fails_with(
        "E0624",
        "ownership is by record; a receipt comes from the receipt store",
        r#"
        use repoweave::vcs::{OwnedRef, RawRefName, ResolvedRevisionId};
        use std::path::PathBuf;
        pub fn forge() -> OwnedRef {
            OwnedRef::from_receipt(
                PathBuf::from("/tmp/store"),
                RawRefName::new("p--ww"),
                ResolvedRevisionId::from_canonical("a".repeat(40), None),
            )
        }
        "#,
    );
}

#[test]
fn a_deletion_warrant_cannot_be_written_by_variant() {
    // A `pub enum`'s variant constructors cannot be made private in Rust,
    // so the warrant is an opaque struct over a private enum and the only
    // constructors are checkers that RUN their check.
    // E0223: `DeletionWarrant` is a struct, so `DeletionWarrant::Unmoved`
    // does not name a variant at all — which is exactly the property a
    // `pub enum` could not have provided.
    assert_fails_with(
        "E0223",
        "a warrant is the output of a check, not a value you fill in",
        r#"
        use repoweave::vcs::DeletionWarrant;
        pub fn forge() -> DeletionWarrant {
            DeletionWarrant::Unmoved { recorded_tip: unimplemented!() }
        }
        "#,
    );
}

// ---------------------------------------------------------------------------
// §4.5 / S13 — an unborn HEAD is not an attachment
// ---------------------------------------------------------------------------

#[test]
fn an_unborn_ref_cannot_be_moved_like_an_attached_one() {
    // MOVE semantics on an unborn HEAD are undefined — a fast-forward
    // merge fails while a reset would stamp the branch into existence — so
    // the call is unrepresentable rather than arbitrarily resolved.
    assert_fails_with(
        "E0308",
        "UnbornRef is a distinct payload from AttachedRef",
        r#"
        use repoweave::vcs::{ResolvedRevisionId, UnbornRef, Vcs};
        pub fn move_unborn(vcs: &dyn Vcs, u: &UnbornRef, to: &ResolvedRevisionId) {
            let _ = vcs.advance_attached_ref(u, to);
        }
        "#,
    );
}

// ---------------------------------------------------------------------------
// §4.6 (3)(4) — deleting a ref you only recognised
// ---------------------------------------------------------------------------

#[test]
fn a_listed_name_cannot_be_deleted() {
    // `list_branch_names_with_prefix` returns raw observed names, so the
    // prefix-glob loop that destroyed a whole set can only report.
    assert_fails_with(
        "E0308",
        "a RawRefName is not a receipt",
        r#"
        use repoweave::vcs::{DeletionWarrant, RawRefName, Vcs};
        pub fn destroy(vcs: &dyn Vcs, name: &RawRefName, w: DeletionWarrant) {
            let _ = vcs.delete_owned_ref(name, w);
        }
        "#,
    );
}

#[test]
fn a_requested_name_cannot_be_deleted() {
    assert_fails_with(
        "E0308",
        "asking for a name is not owning one",
        r#"
        use repoweave::vcs::{DeletionWarrant, EphemeralRefName, Vcs};
        pub fn destroy(vcs: &dyn Vcs, name: &EphemeralRefName, w: DeletionWarrant) {
            let _ = vcs.delete_owned_ref(name, w);
        }
        "#,
    );
}

#[test]
fn a_receipt_alone_is_not_enough_to_delete() {
    assert_fails_with(
        "E0061",
        "the receipt says it is mine; the warrant says it is safe to lose",
        r#"
        use repoweave::vcs::{OwnedRef, Vcs};
        pub fn destroy(vcs: &dyn Vcs, owned: &OwnedRef) {
            let _ = vcs.delete_owned_ref(owned);
        }
        "#,
    );
}

// ---------------------------------------------------------------------------
// §4.6 (6) — minting an ephemeral name from something observed
// ---------------------------------------------------------------------------

#[test]
fn an_ephemeral_name_takes_exactly_two_inputs() {
    // Three sites derived the third component, from three different
    // sources, one of them wrong. The disagreement is not resolved here;
    // it is deleted.
    assert_fails_with(
        "E0061",
        "no third component",
        r#"
        use repoweave::manifest::{ProjectName, WorkweaveName};
        use repoweave::vcs::EphemeralRefName;
        pub fn mint(p: &ProjectName, w: &WorkweaveName) -> EphemeralRefName {
            EphemeralRefName::mint(p, w, "main")
        }
        "#,
    );
}

#[test]
fn an_ephemeral_name_cannot_be_derived_from_an_observed_attachment() {
    assert_fails_with(
        "E0308",
        "a minted name never comes from what a checkout happens to be on",
        r#"
        use repoweave::manifest::ProjectName;
        use repoweave::vcs::{AttachedRef, EphemeralRefName};
        pub fn mint(p: &ProjectName, a: &AttachedRef) -> EphemeralRefName {
            EphemeralRefName::mint(p, a)
        }
        "#,
    );
}

// ---------------------------------------------------------------------------
// §4.3 — consent is a token, not a bool
// ---------------------------------------------------------------------------

#[test]
fn detaching_a_checkout_requires_a_consent_token_nobody_else_can_mint() {
    // `granted()` is the unconditional mint — it checks nothing, so holding
    // its result is the whole proof. It is `#[cfg(test)]`, which means it is
    // absent from the library that this probe (and the `rwv` binary, and
    // every integration test) links against: not merely private, not there.
    // Hence E0599 rather than a privacy error.
    assert_fails_with(
        "E0599",
        "the unconditional mint does not exist outside a test build of the crate",
        r#"
        use repoweave::cli::consent::DetachConsent;
        use repoweave::vcs::{AttachedRef, ResolvedRevisionId, Vcs};
        pub fn detach(vcs: &dyn Vcs, a: &AttachedRef, to: &ResolvedRevisionId) {
            let _ = vcs.detach_head(a, to, DetachConsent::granted());
        }
        "#,
    );
}

#[test]
fn the_flag_mint_is_not_reachable_from_outside_the_cli_module() {
    // The other minting route. `from_flag` is `pub(in crate::cli)`, so this
    // probe pins only half of what that buys: that it is not `pub`. The
    // other half — that `vcs.rs`, inside the crate, cannot call it either —
    // is not observable from an external crate, because every visibility
    // narrower than `pub` looks identical from out here. It is instead a
    // property of where dispatch lives: `cli::dispatch` is the only module
    // in the `cli` tree that mints, and a mint appearing in `vcs.rs` fails
    // `cargo build` with this same E0624.
    assert_fails_with(
        "E0624",
        "consent is minted from a named flag, at dispatch, and nowhere else",
        r#"
        use repoweave::cli::consent::DetachConsent;
        pub fn mint() -> Option<DetachConsent> {
            DetachConsent::from_flag(true)
        }
        "#,
    );
}

// ---------------------------------------------------------------------------
// A consent token cannot be forged outside the flag module
// ---------------------------------------------------------------------------
//
// The two above pin the *functions*: one does not exist outside a test build,
// the other is not `pub`. These pin the claim §4.4 actually requires: even
// the tuple-struct literal — bypassing any constructor function entirely —
// cannot be written, because the field is private to `cli::consent`. That
// privacy rule does not distinguish "a different crate" from "a different
// module of this same crate" (there is no visibility tier between
// plain-private and `pub(crate)` that would), so an external probe
// demonstrating this also demonstrates the in-crate claim: no module of
// `repoweave` other than `cli::consent` — not `vcs.rs`, not `fetch.rs` — can
// write `DetachConsent(())` by hand either.

#[test]
fn a_detach_consent_cannot_be_forged_by_tuple_literal() {
    assert_fails_with(
        "E0423",
        "the field is private to cli::consent; only from_flag/granted can produce one",
        r#"
        use repoweave::cli::consent::DetachConsent;
        pub fn forge() -> DetachConsent {
            DetachConsent(())
        }
        "#,
    );
}

#[test]
fn a_reattach_consent_cannot_be_forged_by_tuple_literal() {
    assert_fails_with(
        "E0423",
        "the field is private to cli::consent; only from_flag/granted can produce one",
        r#"
        use repoweave::cli::consent::ReattachConsent;
        pub fn forge() -> ReattachConsent {
            ReattachConsent(())
        }
        "#,
    );
}

#[test]
fn a_discard_unmerged_consent_cannot_be_forged_by_tuple_literal() {
    assert_fails_with(
        "E0423",
        "the field is private to cli::consent; only from_flag/granted can produce one",
        r#"
        use repoweave::cli::consent::DiscardUnmergedConsent;
        pub fn forge() -> DiscardUnmergedConsent {
            DiscardUnmergedConsent(())
        }
        "#,
    );
}

#[test]
fn an_adopt_detached_consent_cannot_be_forged_by_tuple_literal() {
    // Unlike the other three, this token has no `granted()` — no in-crate
    // fixture needs an unconditional mint yet — so `from_flag` is the only
    // producer.
    assert_fails_with(
        "E0423",
        "the field is private to cli::consent; only from_flag can produce one",
        r#"
        use repoweave::cli::consent::AdoptDetachedConsent;
        pub fn forge() -> AdoptDetachedConsent {
            AdoptDetachedConsent(())
        }
        "#,
    );
}

#[test]
fn a_rewinding_move_requires_a_warrant_argument() {
    assert_fails_with(
        "E0061",
        "a rewind without a savepoint is unrepresentable",
        r#"
        use repoweave::vcs::{AttachedRef, ResolvedRevisionId, Vcs};
        pub fn rewind(vcs: &dyn Vcs, a: &AttachedRef, to: &ResolvedRevisionId) {
            let _ = vcs.reset_attached_ref(a, to);
        }
        "#,
    );
}

// ---------------------------------------------------------------------------
// §4.6 (1) — landing onto a detached target
// ---------------------------------------------------------------------------

#[test]
fn a_witness_cannot_point_a_move_at_a_different_repo() {
    // This is the whole type-level content of §4.6(1), and it is what makes
    // sync-to's detached-target refusal unbypassable rather than merely
    // present. The runtime check on its own leaves a dodge: take the witness
    // from the *cwd* repo — a workweave checkout, so always attached — and
    // keep advancing the target with a path. Every "did you establish there
    // is a branch" gate is then satisfied by an attachment that belongs to
    // the wrong repo.
    //
    // The MOVE derives its repo from the witness and takes no path, so the
    // dodge is not a check someone can route around; it is a call with the
    // wrong arity. Revert `ff_advance_repo` to a path-taking advance and this
    // is the probe that says so.
    assert_fails_with(
        "E0061",
        "a MOVE takes its repo from the witness, never from a path argument",
        r#"
        use repoweave::vcs::{AttachedRef, ResolvedRevisionId, Vcs};
        use std::path::Path;
        pub fn land(
            vcs: &dyn Vcs,
            cwd_witness: &AttachedRef,
            target_repo: &Path,
            to: &ResolvedRevisionId,
        ) {
            let _ = vcs.advance_attached_ref(cwd_witness, target_repo, to);
        }
        "#,
    );
}
