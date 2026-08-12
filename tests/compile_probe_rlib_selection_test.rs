//! `compile_probe::select_rlib` used to reach its "nothing to choose"
//! outcome exactly one way: `filter_map` dropped any name whose `metadata()`
//! or `modified()` call failed, so a listing that raced a rebuild and a
//! listing that matched nothing at all produced the same `None` and the
//! same panic text. A concurrent rebuild holds `librepoweave-*.rlib`
//! unlinked for seconds at a time, so racing one on purpose here would make
//! this suite exactly as flaky as the bug it pins. `select_rlib` takes its
//! stat as a closure instead, so the failure path is driven directly.

mod common;

use common::compile_probe::{select_rlib, RlibSelectionError};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

#[test]
fn no_matching_names_is_reported_as_no_names_matched() {
    let err = select_rlib(Vec::new(), |_| Some(SystemTime::now()))
        .expect_err("an empty candidate list must not select a path");
    assert!(
        matches!(err, RlibSelectionError::NoNamesMatched),
        "expected NoNamesMatched, got {err:?}"
    );
}

#[test]
fn matched_names_whose_stat_all_fail_are_reported_distinctly_from_no_names_matched() {
    let a = PathBuf::from("librepoweave-aaaa.rlib");
    let b = PathBuf::from("librepoweave-bbbb.rlib");
    let err = select_rlib(vec![a.clone(), b.clone()], |_| None)
        .expect_err("a stat failing on every candidate must not select a path");
    match err {
        RlibSelectionError::AllStatsFailed { names } => assert_eq!(names, vec![a, b]),
        other => panic!("expected AllStatsFailed naming both candidates, got {other:?}"),
    }
}

#[test]
fn a_candidate_whose_stat_fails_is_excluded_but_reported_as_skipped_not_dropped() {
    let ok = PathBuf::from("librepoweave-ok.rlib");
    let broken = PathBuf::from("librepoweave-broken.rlib");
    let now = SystemTime::now();
    let ok_for_stat = ok.clone();
    let selection = select_rlib(vec![ok.clone(), broken.clone()], move |p| {
        (p == ok_for_stat.as_path()).then_some(now)
    })
    .expect("one statable candidate is enough to select a path");
    assert_eq!(selection.path, ok);
    assert_eq!(
        selection.skipped,
        vec![broken],
        "the candidate whose stat failed must surface in `skipped`, not vanish"
    );
}

#[test]
fn selection_picks_the_newest_statable_candidate() {
    let old = PathBuf::from("librepoweave-old.rlib");
    let new = PathBuf::from("librepoweave-new.rlib");
    let base = SystemTime::now();
    let new_for_stat = new.clone();
    let selection = select_rlib(vec![old, new.clone()], move |p| {
        Some(if p == new_for_stat.as_path() {
            base + Duration::from_secs(1)
        } else {
            base
        })
    })
    .expect("both candidates stat successfully");
    assert_eq!(selection.path, new);
    assert!(selection.skipped.is_empty());
}
