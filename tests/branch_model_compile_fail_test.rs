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
fn a_found_branch_has_no_as_str_either() {
    assert_fails_with(
        "E0599",
        "a found branch is placed, never compared as a string",
        r#"
        use repoweave::vcs::FoundBranch;
        pub fn spell(found: &FoundBranch) -> &str {
            found.as_str()
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
// No unchecked catch-all ref name
// ---------------------------------------------------------------------------

#[test]
fn there_is_no_ref_name_type_without_a_notion() {
    // The control at the top imports from the same module, so E0432 here is
    // the name being gone, not the path.
    assert_fails_with(
        "E0432",
        "a ref name is one of the refined types or a RawRefName at the parse boundary",
        r#"
        use repoweave::vcs::RefName;
        pub fn name() -> RefName {
            RefName::new("v2.0")
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
fn a_destroy_proof_cannot_be_written_by_literal() {
    // The fields are private to `vcs`, so the proof is not a value a caller
    // can fill in: the only constructors run the check they certify. rustc
    // reports a literal over private fields without an error code, so the
    // refusal is pinned by its message.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::vcs::{DestroyProof, RawRefName, Site};
        use std::path::PathBuf;
        pub fn forge() -> DestroyProof {
            DestroyProof {
                site: Site::Local,
                store: PathBuf::from("/tmp/store"),
                branch: RawRefName::new("main"),
            }
        }
        "#,
    );
    assert!(!compiled, "a proof written by hand must not compile");
    assert!(
        stderr.contains("cannot construct `DestroyProof` with struct literal syntax"),
        "a proof is the output of a check, not a struct a caller fills in; got:\n{stderr}"
    );
}

#[test]
fn a_destroy_proof_has_no_readable_fields() {
    assert_fails_with(
        "E0616",
        "the branch a proof is about is read through the seam, never off the proof",
        r#"
        use repoweave::vcs::DestroyProof;
        pub fn peek(proof: &DestroyProof) {
            let _ = &proof.tip;
        }
        "#,
    );
}

#[test]
fn a_destroy_proof_cannot_be_written_by_variant() {
    // A `pub enum`'s variant constructors cannot be made private in Rust,
    // so the proof is an opaque struct over a private enum and the only
    // constructors are checkers that RUN their check.
    // E0223: `DestroyProof` is a struct, so `DestroyProof::Landed` does not
    // name a variant at all — which is exactly the property a `pub enum`
    // could not have provided.
    assert_fails_with(
        "E0223",
        "a proof is the output of a check, not a value you fill in",
        r#"
        use repoweave::vcs::DestroyProof;
        pub fn forge() -> DestroyProof {
            DestroyProof::Landed { onto: unimplemented!() }
        }
        "#,
    );
}

#[test]
fn the_one_way_to_a_landed_proof_runs_the_check() {
    // Control for the probes around it: the legal route compiles.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::vcs::{DestroyProof, RawRefName, ResolvedRevisionId, Site, Vcs, VcsError};
        use std::path::Path;
        pub fn prove(
            vcs: &dyn Vcs,
            store: &Path,
            branch: &RawRefName,
            onto: &[ResolvedRevisionId],
        ) -> Result<(), VcsError> {
            if let Some(proof) = DestroyProof::landed(vcs, store, Site::Local, branch, onto)? {
                vcs.delete_branch(proof)?;
            }
            Ok(())
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn a_backup_proof_needs_the_operators_consent() {
    assert_fails_with(
        "E0061",
        "keeping a backup and then destroying is the operator's named override",
        r#"
        use repoweave::vcs::{DestroyProof, RawRefName, Site, Vcs};
        use std::path::Path;
        pub fn discard(vcs: &dyn Vcs, store: &Path, branch: &RawRefName) {
            let _ = DestroyProof::backed_up(vcs, store, Site::Local, branch);
        }
        "#,
    );
}

#[test]
fn a_backup_proof_does_not_take_a_bool_for_consent() {
    assert_fails_with(
        "E0308",
        "consent is a token the flag module mints, not true",
        r#"
        use repoweave::vcs::{DestroyProof, RawRefName, Site, Vcs};
        use std::path::Path;
        pub fn discard(vcs: &dyn Vcs, store: &Path, branch: &RawRefName) {
            let _ = DestroyProof::backed_up(vcs, store, Site::Local, branch, true);
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
// §4.6 (3)(4) — deleting a ref you only recognized
// ---------------------------------------------------------------------------

#[test]
fn a_listed_name_cannot_be_deleted() {
    // `list_branch_names_with_prefix` returns raw observed names, so the
    // prefix-glob loop that destroyed a whole set can only report.
    assert_fails_with(
        "E0308",
        "a RawRefName is not a proof",
        r#"
        use repoweave::vcs::{RawRefName, Vcs};
        pub fn destroy(vcs: &dyn Vcs, name: RawRefName) {
            let _ = vcs.delete_branch(name);
        }
        "#,
    );
}

#[test]
fn a_requested_name_cannot_be_deleted() {
    assert_fails_with(
        "E0308",
        "asking for a name is not proving it safe to lose",
        r#"
        use repoweave::vcs::{EphemeralRefName, Vcs};
        pub fn destroy(vcs: &dyn Vcs, name: EphemeralRefName) {
            let _ = vcs.delete_branch(name);
        }
        "#,
    );
}

#[test]
fn a_delete_without_a_proof_does_not_compile() {
    assert_fails_with(
        "E0061",
        "a destroy takes the proof and nothing else, so it cannot be spelled without one",
        r#"
        use repoweave::vcs::Vcs;
        pub fn destroy(vcs: &dyn Vcs) {
            let _ = vcs.delete_branch();
        }
        "#,
    );
}

#[test]
fn there_is_no_destroy_that_takes_a_name() {
    assert_fails_with(
        "E0599",
        "the raw half of a destroy is not on the seam, so no caller can reach it",
        r#"
        use repoweave::vcs::{RawRefName, Vcs};
        use std::path::Path;
        pub fn destroy(vcs: &dyn Vcs, store: &Path, name: &RawRefName) {
            let _ = vcs.destroy_local_ref(store, name);
        }
        "#,
    );
}

#[test]
fn a_birth_takes_the_branch_found_not_a_name() {
    assert_fails_with(
        "E0308",
        "a worktree is placed on a FoundBranch, which carries its store and start point",
        r#"
        use repoweave::vcs::{RawRefName, Vcs};
        use std::path::Path;
        pub fn birth(vcs: &dyn Vcs, name: &RawRefName, dest: &Path) {
            let _ = vcs.create_worktree_on(name, dest);
        }
        "#,
    );
}

// ---------------------------------------------------------------------------
// §4.6 (6) — minting an ephemeral name from something observed
// ---------------------------------------------------------------------------

#[test]
fn an_ephemeral_name_is_minted_from_an_identity_s_flat_address() {
    // Control for the probes below: the one way in compiles.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::manifest::{ProjectName, WorkweaveId, WorkweaveName};
        use repoweave::vcs::EphemeralRefName;
        pub fn mint(project: ProjectName, name: WorkweaveName) -> EphemeralRefName {
            EphemeralRefName::mint(&WorkweaveId { project, name }.flat())
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn an_ephemeral_name_cannot_be_minted_from_text() {
    // Three sites once derived a third component from three different
    // sources. A name minted from text is that shape again.
    assert_fails_with(
        "E0308",
        "the ephemeral name is the identity's flat address, not a string",
        r#"
        use repoweave::vcs::EphemeralRefName;
        pub fn mint(flat: &str) -> EphemeralRefName {
            EphemeralRefName::mint(flat)
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
        use repoweave::vcs::{AttachedRef, EphemeralRefName};
        pub fn mint(a: &AttachedRef) -> EphemeralRefName {
            EphemeralRefName::mint(a)
        }
        "#,
    );
}

#[test]
fn a_flat_address_cannot_be_built_from_text() {
    assert_fails_with(
        "E0423",
        "WorkweaveId::flat is the only producer of a FlatAddress",
        r#"
        use repoweave::naming::FlatAddress;
        pub fn forge(s: String) -> FlatAddress {
            FlatAddress(s)
        }
        "#,
    );
    assert_fails_with(
        "E0277",
        "a FlatAddress has no From<String>",
        r#"
        use repoweave::naming::FlatAddress;
        pub fn forge(s: String) -> FlatAddress {
            s.into()
        }
        "#,
    );
}

// ---------------------------------------------------------------------------
// §4.3 — consent is a token, not a bool
// ---------------------------------------------------------------------------

#[test]
fn detaching_a_checkout_requires_a_departure_proof_nobody_else_can_mint() {
    // `granted()` is the unconditional mint — it checks nothing, so holding
    // its result is the whole proof. It is `#[cfg(test)]`, which means it is
    // absent from the library that this probe (and the `rwv` binary, and
    // every integration test) links against: not merely private, not there.
    // Hence E0599 rather than a privacy error.
    assert_fails_with(
        "E0599",
        "the unconditional mint does not exist outside a test build of the crate",
        r#"
        use repoweave::alignment::Departure;
        use repoweave::vcs::{AttachedRef, ResolvedRevisionId, Vcs};
        pub fn detach(vcs: &dyn Vcs, a: &AttachedRef, to: &ResolvedRevisionId) {
            let _ = vcs.detach_head(a, to, &Departure::granted());
        }
        "#,
    );
}

#[test]
fn a_checkout_cannot_be_detached_without_a_departure_argument() {
    assert_fails_with(
        "E0061",
        "detach_head takes the proof alignment mints, not nothing",
        r#"
        use repoweave::vcs::{AttachedRef, ResolvedRevisionId, Vcs};
        pub fn detach(vcs: &dyn Vcs, a: &AttachedRef, to: &ResolvedRevisionId) {
            let _ = vcs.detach_head(a, to);
        }
        "#,
    );
}

#[test]
fn the_flag_mint_is_not_reachable_from_outside_the_cli_module() {
    // The flag route of a consent token. `from_flag` is `pub(in crate::cli)`,
    // so this probe pins only half of what that buys: that it is not `pub`.
    // The other half — that `vcs.rs`, inside the crate, cannot call it
    // either — is not observable from an external crate, because every
    // visibility narrower than `pub` looks identical from out here. It is
    // instead a property of where dispatch lives: `cli::dispatch` is the only
    // module in the `cli` tree that mints, and a mint appearing in `vcs.rs`
    // fails `cargo build` with this same E0624.
    assert_fails_with(
        "E0624",
        "consent is minted from a named flag, at dispatch, and nowhere else",
        r#"
        use repoweave::cli::consent::DiscardUnlandedConsent;
        pub fn mint() -> Option<DiscardUnlandedConsent> {
            DiscardUnlandedConsent::from_flag(true)
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
// write `DiscardUnlandedConsent(())` by hand either.

#[test]
fn a_departure_cannot_be_forged_by_tuple_literal() {
    assert_fails_with(
        "E0423",
        "the field is private to alignment; only the engine can produce one",
        r#"
        use repoweave::alignment::Departure;
        pub fn forge() -> Departure {
            Departure(())
        }
        "#,
    );
}

#[test]
fn a_discard_unlanded_consent_cannot_be_forged_by_tuple_literal() {
    assert_fails_with(
        "E0423",
        "the field is private to cli::consent; only from_flag/granted can produce one",
        r#"
        use repoweave::cli::consent::DiscardUnlandedConsent;
        pub fn forge() -> DiscardUnlandedConsent {
            DiscardUnlandedConsent(())
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
    // land's detached-target refusal unbypassable rather than merely
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
