//! Tests for the `--role` / `--repo` selector grammar.
//!
//! These exercise `RepoFilter::parse` and `RepoFilter::matches` through the
//! public module boundary so the surface stays stable for downstream verbs
//! (`fetch`, `update`, `push`). Module-internal tests live alongside the
//! implementation in `src/selector.rs`; the per-verb integration tests live
//! in `tests/{fetch,update,push}_test.rs`.

use repoweave::manifest::{RepoPath, Role};
use repoweave::selector::{FilterError, RepoFilter};

fn rp(s: &str) -> RepoPath {
    RepoPath::new(s).expect("test helper: caller must pass forward-slash paths")
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
    let filter = RepoFilter::parse(&["owned".into()], &[]).unwrap();
    assert!(filter.matches(&rp("a"), Role::Owned));
    assert!(!filter.matches(&rp("a"), Role::Dependency));
    assert!(!filter.matches(&rp("a"), Role::Fork));
    assert!(!filter.matches(&rp("a"), Role::Reference));
}

#[test]
fn role_filter_is_case_insensitive() {
    for variant in ["owned", "OWNED", "Owned", "OwNeD"] {
        let filter = RepoFilter::parse(&[variant.into()], &[]).unwrap();
        assert!(filter.matches(&rp("x"), Role::Owned), "case '{variant}'");
    }
}

#[test]
fn union_semantics_role_or_repo_selector() {
    // --role owned --repo github/external/dep
    let filter = RepoFilter::parse(&["owned".into()], &["github/external/dep".into()]).unwrap();
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
        &["owned".into(), "fork".into()],
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

// --- Coverage audit: spot-check gaps ------------------------------------------

/// Globs use globset's `literal_separator(true)` so `?` (single-character
/// wildcard) must not match `/`. Combined with the `*` test above, this pins
/// the "wildcards never cross `/`" contract that selectors inherit.
#[test]
fn glob_question_mark_does_not_cross_slash() {
    let filter = RepoFilter::parse(&[], &["glob:github/org?repo".into()]).unwrap();
    // The `?` cannot match `/`, so `github/org/repo` should NOT match.
    assert!(!filter.matches(&rp("github/org/repo"), Role::Owned));
}

/// Glob character classes (`[ab]`) work and are scoped within one path
/// component — `literal_separator(true)` keeps the class from matching `/`.
#[test]
fn glob_character_class_within_component() {
    let filter = RepoFilter::parse(&[], &["glob:github/[ab]/repo".into()]).unwrap();
    assert!(filter.matches(&rp("github/a/repo"), Role::Owned));
    assert!(filter.matches(&rp("github/b/repo"), Role::Owned));
    assert!(!filter.matches(&rp("github/c/repo"), Role::Owned));
}

/// Regex selectors do NOT inherit `literal_separator` — that's a glob-only
/// option. A regex `.*` matches `/`; pin this so the asymmetric semantics
/// between regex and glob stays documented in tests.
#[test]
fn regex_dot_star_crosses_slash() {
    let filter = RepoFilter::parse(&[], &["re:^github/.*/repo$".into()]).unwrap();
    assert!(filter.matches(&rp("github/a/repo"), Role::Owned));
    assert!(filter.matches(&rp("github/a/b/repo"), Role::Owned));
    assert!(!filter.matches(&rp("github/a/other"), Role::Owned));
}

/// Regex selectors are **unanchored by default** — the regex crate does
/// substring matching unless the pattern uses `^` / `$`. This is the same
/// behaviour as `grep -E`; pin it explicitly so docs that show `re:foo` can
/// rely on substring semantics.
#[test]
fn regex_unanchored_by_default() {
    let filter = RepoFilter::parse(&[], &["re:cwalv".into()]).unwrap();
    assert!(filter.matches(&rp("github/cwalv/repoweave"), Role::Owned));
    assert!(filter.matches(&rp("anywhere/cwalv-anything"), Role::Owned));
    assert!(!filter.matches(&rp("github/other/repo"), Role::Owned));
}

/// Special characters in exact-match selectors (no prefix) are not
/// interpreted — they're compared as plain strings. Pin this so a path that
/// looks regexy doesn't accidentally invoke a regex code path.
#[test]
fn exact_selector_does_not_interpret_special_characters() {
    let filter = RepoFilter::parse(&[], &["github/cwalv/*".into()]).unwrap();
    // Treated as exact — only matches the literal path with a `*`.
    assert!(!filter.matches(&rp("github/cwalv/anything"), Role::Owned));
}

/// Whitespace and unicode in regex/glob patterns: the parsers don't trim
/// or normalise; pin the verbatim-match contract.
#[test]
fn glob_with_unicode_passes_through() {
    let filter = RepoFilter::parse(&[], &["glob:github/café/*".into()]).unwrap();
    assert!(filter.matches(&rp("github/café/repo"), Role::Owned));
    assert!(!filter.matches(&rp("github/cafe/repo"), Role::Owned));
}

/// The legacy `--role primary` error must name the spelling that replaced it
/// — the migration `docs/reference/roles.md` describes. Mirrors the
/// inline test in `src/selector.rs` so external callers see it via the public
/// API too.
#[test]
fn legacy_role_primary_names_the_replacement_spelling() {
    let err = RepoFilter::parse(&[Role::LEGACY_SPELLING.into()], &[]).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains(&Role::legacy_spelling_hint()),
        "legacy --role primary must surface the migration sentence, got: {msg}"
    );
}
