//! Tests for the `--role` / `--repo` selector grammar (fo-9kweo).
//!
//! These exercise `RepoFilter::parse` and `RepoFilter::matches` through the
//! public module boundary so the surface stays stable for downstream verbs
//! (`fetch`, `update`, `push`). Module-internal tests live alongside the
//! implementation in `src/selector.rs`; the per-verb integration tests live
//! in `tests/{fetch,update,push}_test.rs`.

use repoweave::manifest::{RepoPath, Role};
use repoweave::selector::{FilterError, RepoFilter};

fn rp(s: &str) -> RepoPath {
    RepoPath::new(s)
}

#[test]
fn empty_filter_passes_everything() {
    let filter = RepoFilter::parse(&[], &[]).expect("empty parse");
    assert!(filter.is_empty());
    assert!(filter.matches(&rp("github/a/b"), Role::Owned));
    assert!(filter.matches(&rp("anywhere"), Role::Reference));
}

#[test]
fn exact_selector_matches_full_path_only() {
    let filter = RepoFilter::parse(&[], &["github/cwalv/repoweave".into()]).unwrap();
    assert!(!filter.is_empty());
    assert!(filter.matches(&rp("github/cwalv/repoweave"), Role::Owned));
    assert!(!filter.matches(&rp("github/cwalv/repoweave-extra"), Role::Owned));
    assert!(!filter.matches(&rp("repoweave"), Role::Owned));
}

#[test]
fn regex_selector_matches_via_re_prefix() {
    let filter = RepoFilter::parse(&[], &["re:^github/(cwalv|other)/".into()]).unwrap();
    assert!(filter.matches(&rp("github/cwalv/x"), Role::Owned));
    assert!(filter.matches(&rp("github/other/y"), Role::Dependency));
    assert!(!filter.matches(&rp("gitlab/cwalv/x"), Role::Owned));
}

#[test]
fn glob_selector_matches_via_glob_prefix() {
    let filter = RepoFilter::parse(&[], &["glob:github/org/*".into()]).unwrap();
    assert!(filter.matches(&rp("github/org/foo"), Role::Owned));
    assert!(filter.matches(&rp("github/org/bar"), Role::Reference));
    // Single * doesn't cross a `/` boundary (literal_separator true).
    assert!(!filter.matches(&rp("github/org/sub/deep"), Role::Owned));
    assert!(!filter.matches(&rp("github/other/foo"), Role::Owned));
}

#[test]
fn glob_double_star_crosses_slashes() {
    let filter = RepoFilter::parse(&[], &["glob:github/**".into()]).unwrap();
    assert!(filter.matches(&rp("github/a"), Role::Owned));
    assert!(filter.matches(&rp("github/a/b/c"), Role::Owned));
}

#[test]
fn role_filter_includes_matching_role_only() {
    let filter = RepoFilter::parse(&["primary".into()], &[]).unwrap();
    assert!(filter.matches(&rp("a"), Role::Owned));
    assert!(!filter.matches(&rp("a"), Role::Dependency));
    assert!(!filter.matches(&rp("a"), Role::Fork));
    assert!(!filter.matches(&rp("a"), Role::Reference));
}

#[test]
fn role_filter_is_case_insensitive() {
    for variant in ["primary", "PRIMARY", "Primary", "PrImArY"] {
        let filter = RepoFilter::parse(&[variant.into()], &[]).unwrap();
        assert!(filter.matches(&rp("x"), Role::Owned), "case '{variant}'");
    }
}

#[test]
fn union_semantics_role_or_repo_selector() {
    // --role primary --repo github/external/dep
    let filter = RepoFilter::parse(&["primary".into()], &["github/external/dep".into()]).unwrap();
    // Matches because of role.
    assert!(filter.matches(&rp("github/me/code"), Role::Owned));
    // Matches because of selector even though role differs.
    assert!(filter.matches(&rp("github/external/dep"), Role::Dependency));
    // Neither role nor path match → excluded.
    assert!(!filter.matches(&rp("github/other/dep"), Role::Dependency));
}

#[test]
fn multiple_of_each_kind_accumulate_as_union() {
    let filter = RepoFilter::parse(
        &["primary".into(), "fork".into()],
        &[
            "github/a/exact".into(),
            "re:^lib/".into(),
            "glob:vendor/*/proj".into(),
        ],
    )
    .unwrap();
    // role matches
    assert!(filter.matches(&rp("any/path"), Role::Owned));
    assert!(filter.matches(&rp("any/path"), Role::Fork));
    // exact match
    assert!(filter.matches(&rp("github/a/exact"), Role::Reference));
    // regex
    assert!(filter.matches(&rp("lib/foo"), Role::Dependency));
    // glob
    assert!(filter.matches(&rp("vendor/acme/proj"), Role::Reference));
    // nothing matches
    assert!(!filter.matches(&rp("totally/unrelated"), Role::Dependency));
}

#[test]
fn empty_regex_pattern_after_prefix_is_parse_error() {
    let err = RepoFilter::parse(&[], &["re:".into()]).unwrap_err();
    let msg = format!("{err}");
    assert!(matches!(err, FilterError::EmptyPattern { kind: "re" }));
    assert!(msg.contains("empty"), "got: {msg}");
}

#[test]
fn empty_glob_pattern_after_prefix_is_parse_error() {
    let err = RepoFilter::parse(&[], &["glob:".into()]).unwrap_err();
    assert!(matches!(err, FilterError::EmptyPattern { kind: "glob" }));
}

#[test]
fn invalid_regex_is_parse_error() {
    let err = RepoFilter::parse(&[], &["re:[unclosed".into()]).unwrap_err();
    assert!(matches!(err, FilterError::InvalidRegex { .. }));
    let msg = format!("{err}");
    assert!(msg.contains("invalid regex"), "got: {msg}");
}

#[test]
fn invalid_glob_is_parse_error() {
    let err = RepoFilter::parse(&[], &["glob:[unclosed".into()]).unwrap_err();
    assert!(matches!(err, FilterError::InvalidGlob { .. }));
}

#[test]
fn unknown_role_is_parse_error() {
    let err = RepoFilter::parse(&["mainline".into()], &[]).unwrap_err();
    assert!(matches!(err, FilterError::UnknownRole(_)));
    let msg = format!("{err}");
    assert!(msg.contains("mainline"), "got: {msg}");
}
