//! The branch model's type-level invariants, pinned **by error code**
//! (`docs/repoweave/branch-model.md` §4.7).
//!
//! The types carry `compile_fail` doctests, mirroring the precedent
//! `RawRevisionId` set. Those are the enforcement that survives a refactor
//! — they fail in CI the day someone adds the `PartialEq` or the `From`
//! impl to "make it easier". What they do **not** do is check *why* the
//! snippet failed: on stable, rustdoc accepts the `Exxxx` annotation and
//! ignores it, so a `compile_fail,E0599` doctest passes when the snippet
//! fails with an unrelated E0308 — or with a typo.
//!
//! That gap matters here more than usual. §4.2 removes `as_str()` from
//! three types specifically so the two shipped comparison lines in
//! `push.rs` stop compiling, and the claim is that they fail with **E0599,
//! no such method** rather than merely "somehow". A doctest cannot make
//! that claim; this can. Each case below compiles a snippet against the
//! built library with `rustc` and asserts the exact diagnostic code.
//!
//! The first test is a control that must **succeed**: without it, a broken
//! invocation would make every other assertion pass vacuously.

use std::path::{Path, PathBuf};
use std::process::Command;

/// `target/<profile>/deps`, where the compiled library and its
/// dependencies' metadata live.
fn deps_dir() -> PathBuf {
    Path::new(env!("CARGO_BIN_EXE_rwv"))
        .parent()
        .expect("the test binary lives under target/<profile>")
        .join("deps")
}

/// The freshest `librepoweave-*.rlib` in the deps directory.
///
/// Stale artifacts from earlier builds accumulate there, so pick by
/// modification time rather than by first match.
fn repoweave_rlib() -> PathBuf {
    let deps = deps_dir();
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&deps)
        .unwrap_or_else(|e| panic!("read {}: {e}", deps.display()))
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("librepoweave-") && n.ends_with(".rlib"))
        })
        .filter_map(|p| Some((p.metadata().ok()?.modified().ok()?, p)))
        .collect();
    candidates.sort_by_key(|(modified, _)| *modified);
    candidates
        .pop()
        .map(|(_, p)| p)
        .unwrap_or_else(|| panic!("no librepoweave-*.rlib in {}", deps.display()))
}

/// Compile `snippet` as a library against the built `repoweave`, returning
/// `(compiled, stderr)`.
///
/// `--emit=metadata` stops before codegen: the snippets exist to be
/// type-checked, and nothing here needs to link or run.
fn compile(snippet: &str) -> (bool, String) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("probe.rs");
    std::fs::write(&src, snippet).expect("write probe");

    let out = Command::new("rustc")
        .arg("--edition=2021")
        .arg("--crate-type=lib")
        .arg("--emit=metadata")
        .arg("--out-dir")
        .arg(tmp.path())
        .arg("--extern")
        .arg(format!("repoweave={}", repoweave_rlib().display()))
        .arg("-L")
        .arg(format!("dependency={}", deps_dir().display()))
        .arg(&src)
        .output()
        .expect("run rustc");

    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Assert `snippet` fails to compile with exactly `code`, and say what it
/// did instead when it does not.
fn assert_fails_with(code: &str, what: &str, snippet: &str) {
    assert_fails_n_times(code, 1, what, snippet);
}

/// As [`assert_fails_with`], but requiring `code` at least `n` times.
///
/// A snippet that violates the same invariant on both sides of an operator
/// emits one error per side, and a `contains` check is satisfied by either
/// one alone — so such a snippet keeps failing after half the invariant is
/// gone. Where the count is the point, it is asserted.
fn assert_fails_n_times(code: &str, n: usize, what: &str, snippet: &str) {
    let (compiled, stderr) = compile(snippet);
    assert!(
        !stderr.contains("error[E0514]"),
        "{what}: the probe compiler disagrees with the one that built the \
         library, so every assertion below would pass for the wrong reason:\n{stderr}"
    );
    assert!(
        !compiled,
        "{what}: expected {code}, but the snippet COMPILED — the invariant \
         is not enforced"
    );
    let seen = stderr.matches(&format!("error[{code}]")).count();
    assert!(
        seen >= n,
        "{what}: expected {code} at least {n}x, saw it {seen}x in:\n{stderr}"
    );
}

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
    assert_fails_with(
        "E0624",
        "consent is minted from a named flag, not conjured at the call site",
        r#"
        use repoweave::vcs::{AttachedRef, DetachConsent, ResolvedRevisionId, Vcs};
        pub fn detach(vcs: &dyn Vcs, a: &AttachedRef, to: &ResolvedRevisionId) {
            let _ = vcs.detach_head(a, to, DetachConsent::granted());
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
