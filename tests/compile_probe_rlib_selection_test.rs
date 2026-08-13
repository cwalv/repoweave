//! `compile_probe::select_rlib` used to reach its "nothing to choose"
//! outcome exactly one way: `filter_map` dropped any name whose `metadata()`
//! or `modified()` call failed, so a listing that raced a rebuild and a
//! listing that matched nothing at all produced the same `None` and the
//! same panic text. rwv-j3qm split those outcomes and added a `stat` seam so
//! the failure path could be driven directly instead of by racing a real
//! rebuild.
//!
//! rwv-1uto is the correctness fix that goes further: mtime is dropped as a
//! selection input entirely because it cannot know which rlib the running
//! test binary was actually linked against. Selection is now keyed on the
//! mangled `Cs...repoweave` crate identity rustc bakes into every symbol —
//! the exact ID appears in both the test binary and the one rlib it links.
//! The tests below drive the seam directly, including the planted-stale
//! case (a wrong-variant rlib newer than the correct one) that motivated
//! the fix.

mod common;

use common::compile_probe::{
    extract_crate_identity, select_rlib, CrateIdentity, RlibSelectionError,
};
use std::path::PathBuf;

/// Build a `Cs<id>_9repoweave` marker for a synthetic base62 id.
fn identity(id: &str) -> CrateIdentity {
    let mut bytes = b"Cs".to_vec();
    bytes.extend_from_slice(id.as_bytes());
    bytes.extend_from_slice(b"_9repoweave");
    CrateIdentity(bytes)
}

#[test]
fn no_matching_names_is_reported_as_no_names_matched() {
    let wanted = identity("abcdefghij0");
    let err = select_rlib(Vec::new(), &wanted, |_| Some(identity("abcdefghij0")))
        .expect_err("an empty candidate list must not select a path");
    assert!(
        matches!(err, RlibSelectionError::NoNamesMatched),
        "expected NoNamesMatched, got {err:?}"
    );
}

#[test]
fn matched_names_whose_identity_read_all_fail_are_reported_distinctly_from_no_names_matched() {
    let a = PathBuf::from("librepoweave-aaaa.rlib");
    let b = PathBuf::from("librepoweave-bbbb.rlib");
    let wanted = identity("abcdefghij0");
    let err = select_rlib(vec![a.clone(), b.clone()], &wanted, |_| None)
        .expect_err("an unreadable identity on every candidate must not select a path");
    match err {
        RlibSelectionError::AllIdentitiesUnreadable { names } => assert_eq!(names, vec![a, b]),
        other => panic!("expected AllIdentitiesUnreadable naming both candidates, got {other:?}"),
    }
}

#[test]
fn a_candidate_whose_identity_is_unreadable_is_excluded_but_reported_as_skipped_not_dropped() {
    let ok = PathBuf::from("librepoweave-ok.rlib");
    let broken = PathBuf::from("librepoweave-broken.rlib");
    let wanted = identity("okokokokok0");
    let ok_for_read = ok.clone();
    let wanted_for_read = wanted.clone();
    let selection = select_rlib(vec![ok.clone(), broken.clone()], &wanted, move |p| {
        (p == ok_for_read.as_path()).then(|| wanted_for_read.clone())
    })
    .expect("one identifiable candidate carrying the wanted id is enough to select a path");
    assert_eq!(selection.path, ok);
    assert_eq!(
        selection.skipped,
        vec![broken],
        "the candidate whose identity read failed must surface in `skipped`, not vanish"
    );
}

/// The core acceptance case for the crate-identity keying: a wrong-variant
/// rlib planted with a newer mtime than the correct one must NOT be
/// selected. Under the old mtime policy the wrong variant would win here;
/// keying on crate identity makes that outcome unrepresentable.
///
/// The point of the fix is that mtime is no longer consulted at any layer of
/// the selector. The test binds the two identities to the two paths and
/// asserts the correct one wins regardless of any timestamp the surrounding
/// world could plant.
#[test]
fn a_newer_wrong_variant_rlib_is_refused_not_preferred() {
    // "Build variant" is the stale one that would have been picked under
    // the old mtime policy — it exists on disk, and is newer, but the test
    // binary was not linked against it.
    let build_variant = PathBuf::from("librepoweave-BUILDvariant.rlib");
    // "Test variant" is what rustc actually linked the running test binary
    // against. Selection must land on this one.
    let test_variant = PathBuf::from("librepoweave-TESTvariant.rlib");

    let build_id = identity("BUILDid0001");
    let test_id = identity("TESTid00001");

    let build_variant_for_read = build_variant.clone();
    let test_variant_for_read = test_variant.clone();
    let build_id_for_read = build_id.clone();
    let test_id_for_read = test_id.clone();

    let selection = select_rlib(
        // Order the candidate list with the stale one FIRST so a bug that
        // fell back to "first match wins" would also pick wrong.
        vec![build_variant.clone(), test_variant.clone()],
        &test_id,
        move |p| {
            if p == build_variant_for_read.as_path() {
                Some(build_id_for_read.clone())
            } else if p == test_variant_for_read.as_path() {
                Some(test_id_for_read.clone())
            } else {
                None
            }
        },
    )
    .expect("the wanted id is present on exactly one candidate — selection must succeed");
    assert_eq!(
        selection.path, test_variant,
        "selection must pick the rlib whose crate identity matches the running \
         test binary's, not any other — this is the whole point of rwv-1uto"
    );
    assert!(
        selection.skipped.is_empty(),
        "a wrong-identity candidate is not `skipped` (skipped is for unreadable \
         identities); it is filtered out silently and surfaces only in \
         NoIdentityMatch's `candidates` when nothing matches at all"
    );
}

/// Companion to the planted-stale case: when the running test binary's
/// identity is present on ZERO candidates (i.e. the linked artifact isn't
/// there and the only rlibs on disk are wrong-variant), selection must
/// refuse with a message that names the event. The old mtime policy would
/// have masked this by picking any stale rlib present; keying on identity
/// makes it a first-class refusal.
#[test]
fn zero_matching_identities_refuses_with_wanted_and_candidates_named() {
    let a = PathBuf::from("librepoweave-aaaa.rlib");
    let b = PathBuf::from("librepoweave-bbbb.rlib");
    let a_id = identity("Aid00000000");
    let b_id = identity("Bid00000000");
    let wanted = identity("WANTedid001");

    let a_for_read = a.clone();
    let b_for_read = b.clone();
    let a_id_for_read = a_id.clone();
    let b_id_for_read = b_id.clone();

    let err = select_rlib(vec![a.clone(), b.clone()], &wanted, move |p| {
        if p == a_for_read.as_path() {
            Some(a_id_for_read.clone())
        } else if p == b_for_read.as_path() {
            Some(b_id_for_read.clone())
        } else {
            None
        }
    })
    .expect_err("no candidate carries the wanted identity — selection must refuse");
    match err {
        RlibSelectionError::NoIdentityMatch {
            wanted: got_wanted,
            candidates,
        } => {
            assert_eq!(got_wanted, wanted, "refusal must name the wanted identity");
            assert_eq!(
                candidates,
                vec![(a, Some(a_id)), (b, Some(b_id))],
                "refusal must list every candidate and the identity it did carry \
                 so the operator can see what was on disk vs. what was wanted"
            );
        }
        other => panic!("expected NoIdentityMatch, got {other:?}"),
    }
}

/// When two rlibs claim the same identity as the test binary, selection
/// must refuse rather than coin-flip. Two artifacts sharing a StableCrateId
/// is a broken toolchain invariant; picking one silently would let the
/// probe pass while proving something about an unrelated file.
#[test]
fn two_candidates_with_same_wanted_identity_refuses_with_both_named() {
    let a = PathBuf::from("librepoweave-aaaa.rlib");
    let b = PathBuf::from("librepoweave-bbbb.rlib");
    let wanted = identity("DUPid000000");
    let wanted_for_read = wanted.clone();

    let err = select_rlib(vec![a.clone(), b.clone()], &wanted, move |_| {
        Some(wanted_for_read.clone())
    })
    .expect_err("two candidates carrying the same wanted identity must not pick a winner");
    match err {
        RlibSelectionError::AmbiguousIdentityMatch {
            wanted: got_wanted,
            matched,
        } => {
            assert_eq!(got_wanted, wanted);
            assert_eq!(matched, vec![a, b], "both matched paths must be named");
        }
        other => panic!("expected AmbiguousIdentityMatch, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// extract_crate_identity — the byte-scan primitive
// ---------------------------------------------------------------------------
//
// select_rlib is only sound if the primitive it composes actually finds the
// crate identity rustc bakes into a binary. These tests pin the parser
// against synthetic byte inputs shaped like the real thing.

#[test]
fn extract_finds_a_single_embedded_marker() {
    let bytes = b"prelude junk Cs2WVz16tfe57_9repoweave more junk";
    let id = extract_crate_identity(bytes).expect("one marker is present");
    assert_eq!(id, identity("2WVz16tfe57"));
}

#[test]
fn extract_returns_none_when_no_marker_is_present() {
    let bytes = b"prelude junk without any repoweave marker at all";
    match extract_crate_identity(bytes) {
        Err(None) => {}
        other => panic!("expected Err(None), got {other:?}"),
    }
}

#[test]
fn extract_reports_multiple_distinct_markers() {
    // A binary linked against two repoweave crates (different StableCrateIds)
    // would surface as two markers; the primitive must not pick one.
    let bytes = b"Cs2WVz16tfe57_9repoweave and also Cs2oYmEFI80sP_9repoweave here";
    match extract_crate_identity(bytes) {
        Err(Some(all)) => {
            assert_eq!(all.len(), 2);
            let strings: Vec<String> = all.iter().map(|id| id.to_string()).collect();
            assert!(strings.contains(&"Cs2WVz16tfe57_9repoweave".to_string()));
            assert!(strings.contains(&"Cs2oYmEFI80sP_9repoweave".to_string()));
        }
        other => panic!("expected Err(Some(_)) with two ids, got {other:?}"),
    }
}

#[test]
fn extract_deduplicates_repeated_occurrences_of_the_same_marker() {
    // Every symbol from the crate carries the marker, so a real binary has
    // thousands of copies of the SAME marker. That is one identity, not
    // many, and must not trip the "multiple distinct markers" refusal.
    let bytes = b"Cs2WVz16tfe57_9repoweave x2: Cs2WVz16tfe57_9repoweave";
    let id = extract_crate_identity(bytes).expect("one distinct marker despite two occurrences");
    assert_eq!(id, identity("2WVz16tfe57"));
}

#[test]
fn extract_rejects_a_truncated_marker_with_no_suffix() {
    // A `Cs<base62>` prefix without `_9repoweave` following is not a
    // marker for this crate — the scanner must not accept it.
    let bytes = b"Cs2WVz16tfe57_10otherCrate here, no repoweave in sight";
    match extract_crate_identity(bytes) {
        Err(None) => {}
        other => panic!("expected Err(None) — no repoweave marker, got {other:?}"),
    }
}
