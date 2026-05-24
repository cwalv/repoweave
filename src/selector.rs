//! Shared `--role` / `--repo` selector grammar for `rwv fetch`, `rwv update`,
//! and `rwv push` (fo-9kweo).
//!
//! A [`RepoFilter`] narrows a verb's per-repo loop to a subset of the manifest.
//! Construction is once-per-invocation via [`RepoFilter::parse`]; the caller
//! then asks `filter.matches(path, role)` for each candidate repo.
//!
//! ## CLI surface
//!
//! Each verb gains two repeated args:
//!
//! ```text
//! rwv <verb> [--role ROLE]... [--repo SELECTOR]...
//! ```
//!
//! `--role` values are the existing [`Role`] variants (`primary`, `dependency`,
//! `fork`, `reference`), case-insensitive.
//!
//! `--repo` selectors dispatch on prefix:
//!
//! - bare string (no prefix) — exact match on the manifest path.
//! - `re:<pattern>` — regex match on the manifest path. Anchoring is the
//!   pattern's responsibility (use `^` / `$` if you need it).
//! - `glob:<pattern>` — glob match on the manifest path. Patterns are
//!   evaluated with `globset` in `literal_separator(true)` mode so `*` does
//!   not cross `/` boundaries (matches how `.gitignore` / ripgrep handle
//!   path globs).
//!
//! ## Semantics
//!
//! - **Empty filter** (no `--role`, no `--repo`): every repo passes. Existing
//!   verb behaviour is preserved bit-for-bit.
//! - **Union accumulation:** a repo is included if it matches *any* `--role`
//!   value OR *any* `--repo` selector. This matches the common case of
//!   "these specific repos plus everything in this role" (see fo-9kweo
//!   "Open questions" — resolved as union).
//! - **Repeated flags only** for multi-value forms (no comma-splitting). Pattern
//!   bodies for `re:` / `glob:` legitimately contain commas, so comma-as-
//!   separator would be a footgun.
//!
//! ## Lock-precondition note (push specifically)
//!
//! The push verb's lock-vs-state precondition (`fo-nxba7`) checks the *full*
//! manifest, not the filtered subset. The committed lock describes every
//! manifest repo; publishing a project-repo lock that doesn't match the
//! unfiltered repos breaks collaborators. See the comment at the
//! lock-precondition site in `src/push.rs`. The filter narrows the push
//! loop, never the precondition.

use crate::manifest::{RepoPath, Role};
use globset::{Glob, GlobMatcher};
use regex::Regex;
use std::fmt;
use std::str::FromStr;

/// Parsed `--role` / `--repo` filter. Cheap to clone? No — owns compiled
/// `Regex` and `GlobMatcher` values. Built once per verb invocation and
/// passed by reference into the per-repo loop.
#[derive(Debug, Clone)]
pub struct RepoFilter {
    roles: Vec<Role>,
    selectors: Vec<RepoSelector>,
}

/// One parsed `--repo` argument. Constructed via [`RepoFilter::parse`].
///
/// The `pattern` strings on `Regex` / `Glob` are kept verbatim for
/// future diagnostic surfacing (e.g. a "selector matched nothing" warning).
/// They aren't read on the hot path today — `#[allow(dead_code)]` rather than
/// stripping them, because the pattern source is the natural thing to surface
/// in any future diagnostic and dropping it now would mean reconstructing
/// from compiled state.
#[derive(Debug, Clone)]
enum RepoSelector {
    /// Bare string — exact match on the manifest path.
    Exact(String),
    /// `re:<pattern>` — regex match.
    Regex {
        #[allow(dead_code)]
        pattern: String,
        re: Regex,
    },
    /// `glob:<pattern>` — glob match.
    Glob {
        #[allow(dead_code)]
        pattern: String,
        matcher: GlobMatcher,
    },
}

/// Errors from [`RepoFilter::parse`]. Surfaced verbatim from each verb so the
/// CLI user sees the offending flag value in the error.
#[derive(Debug)]
pub enum FilterError {
    /// `--role <value>` did not match any [`Role`] variant.
    UnknownRole(String),
    /// `--repo re:` or `--repo glob:` with nothing after the prefix.
    EmptyPattern { kind: &'static str },
    /// `--repo re:<pattern>` where `<pattern>` failed to compile as a regex.
    InvalidRegex {
        pattern: String,
        source: regex::Error,
    },
    /// `--repo glob:<pattern>` where `<pattern>` failed to compile as a glob.
    InvalidGlob {
        pattern: String,
        source: globset::Error,
    },
}

impl fmt::Display for FilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownRole(value) => write!(
                f,
                "--role: '{value}' is not a recognised role (expected primary, dependency, fork, or reference)"
            ),
            Self::EmptyPattern { kind } => {
                write!(f, "--repo {kind}: pattern is empty after the '{kind}:' prefix")
            }
            Self::InvalidRegex { pattern, source } => {
                write!(f, "--repo re:{pattern}: invalid regex: {source}")
            }
            Self::InvalidGlob { pattern, source } => {
                write!(f, "--repo glob:{pattern}: invalid glob: {source}")
            }
        }
    }
}

impl std::error::Error for FilterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidRegex { source, .. } => Some(source),
            Self::InvalidGlob { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl RepoFilter {
    /// Build a filter from raw CLI args. `roles` are the `--role` values as
    /// supplied (case-insensitive — they go through [`Role::from_str`]);
    /// `selectors` are the `--repo` values with their prefix (or no prefix
    /// for exact match) intact.
    ///
    /// On the first invalid input, returns an error that names the offending
    /// flag value. The CLI surfaces this verbatim.
    pub fn parse(roles: &[String], selectors: &[String]) -> Result<Self, FilterError> {
        let mut parsed_roles: Vec<Role> = Vec::with_capacity(roles.len());
        for raw in roles {
            parsed_roles.push(parse_role(raw)?);
        }

        let mut parsed_selectors: Vec<RepoSelector> = Vec::with_capacity(selectors.len());
        for raw in selectors {
            parsed_selectors.push(parse_selector(raw)?);
        }

        Ok(Self {
            roles: parsed_roles,
            selectors: parsed_selectors,
        })
    }

    /// True iff both `--role` and `--repo` lists are empty — callers should
    /// short-circuit (every repo passes) without invoking [`Self::matches`]
    /// per item.
    pub fn is_empty(&self) -> bool {
        self.roles.is_empty() && self.selectors.is_empty()
    }

    /// Does `(path, role)` pass the filter?
    ///
    /// An empty filter passes everything. Otherwise the repo passes if its
    /// role appears in any `--role` flag OR its path matches any `--repo`
    /// selector (union, not intersection).
    pub fn matches(&self, path: &RepoPath, role: Role) -> bool {
        if self.is_empty() {
            return true;
        }
        if self.roles.contains(&role) {
            return true;
        }
        let p = path.as_str();
        self.selectors.iter().any(|s| match s {
            RepoSelector::Exact(want) => p == want.as_str(),
            RepoSelector::Regex { re, .. } => re.is_match(p),
            RepoSelector::Glob { matcher, .. } => matcher.is_match(p),
        })
    }
}

/// Case-insensitive parse of a `--role` value against the [`Role`] enum.
///
/// We don't use clap's `ValueEnum` parsing here because `--role` is collected
/// as `Vec<String>` in the verb subcommands (shared across three verbs; the
/// arg is wired up identically per verb). Going through `Role::from_str`
/// keeps the parse error type ours so the CLI surfaces a consistent message.
fn parse_role(raw: &str) -> Result<Role, FilterError> {
    Role::from_str(raw).map_err(|_| FilterError::UnknownRole(raw.to_string()))
}

fn parse_selector(raw: &str) -> Result<RepoSelector, FilterError> {
    if let Some(body) = raw.strip_prefix("re:") {
        if body.is_empty() {
            return Err(FilterError::EmptyPattern { kind: "re" });
        }
        let re = Regex::new(body).map_err(|source| FilterError::InvalidRegex {
            pattern: body.to_string(),
            source,
        })?;
        Ok(RepoSelector::Regex {
            pattern: body.to_string(),
            re,
        })
    } else if let Some(body) = raw.strip_prefix("glob:") {
        if body.is_empty() {
            return Err(FilterError::EmptyPattern { kind: "glob" });
        }
        let glob = Glob::new(body).map_err(|source| FilterError::InvalidGlob {
            pattern: body.to_string(),
            source,
        })?;
        // literal_separator(true) is built into the default GlobBuilder for
        // path matchers — wildcards don't cross `/`. We construct via
        // GlobBuilder to make the intent explicit.
        let matcher = globset::GlobBuilder::new(body)
            .literal_separator(true)
            .build()
            .map_err(|source| FilterError::InvalidGlob {
                pattern: body.to_string(),
                source,
            })?
            .compile_matcher();
        // `glob` is built above to surface a parse-side error early; the
        // GlobBuilder-built matcher is what we actually use.
        let _ = glob;
        Ok(RepoSelector::Glob {
            pattern: body.to_string(),
            matcher,
        })
    } else {
        Ok(RepoSelector::Exact(raw.to_string()))
    }
}

// Role doesn't expose a FromStr today; provide one here gated on the same
// `as_str` variants the manifest already serialises. Case-insensitive.
impl FromStr for Role {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "primary" => Ok(Role::Primary),
            "fork" => Ok(Role::Fork),
            "dependency" => Ok(Role::Dependency),
            "reference" => Ok(Role::Reference),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rp(s: &str) -> RepoPath {
        RepoPath::new(s)
    }

    #[test]
    fn empty_filter_matches_everything() {
        let f = RepoFilter::parse(&[], &[]).unwrap();
        assert!(f.is_empty());
        assert!(f.matches(&rp("github/cwalv/repoweave"), Role::Primary));
        assert!(f.matches(&rp("anywhere/else"), Role::Reference));
    }

    #[test]
    fn parse_role_is_case_insensitive() {
        let f = RepoFilter::parse(&["Primary".into()], &[]).unwrap();
        assert!(f.matches(&rp("x"), Role::Primary));
        let f = RepoFilter::parse(&["DEPENDENCY".into()], &[]).unwrap();
        assert!(f.matches(&rp("x"), Role::Dependency));
    }

    #[test]
    fn parse_role_rejects_unknown() {
        let err = RepoFilter::parse(&["bogus".into()], &[]).unwrap_err();
        match err {
            FilterError::UnknownRole(v) => assert_eq!(v, "bogus"),
            other => panic!("expected UnknownRole, got {other}"),
        }
    }

    #[test]
    fn role_filter_includes_matching_role_excludes_others() {
        let f = RepoFilter::parse(&["primary".into()], &[]).unwrap();
        assert!(f.matches(&rp("any/path"), Role::Primary));
        assert!(!f.matches(&rp("any/path"), Role::Dependency));
        assert!(!f.matches(&rp("any/path"), Role::Fork));
        assert!(!f.matches(&rp("any/path"), Role::Reference));
    }

    #[test]
    fn multiple_roles_accumulate_as_union() {
        let f = RepoFilter::parse(&["primary".into(), "fork".into()], &[]).unwrap();
        assert!(f.matches(&rp("x"), Role::Primary));
        assert!(f.matches(&rp("x"), Role::Fork));
        assert!(!f.matches(&rp("x"), Role::Reference));
    }

    #[test]
    fn exact_selector_matches_only_that_path() {
        let f = RepoFilter::parse(&[], &["github/cwalv/repoweave".into()]).unwrap();
        assert!(f.matches(&rp("github/cwalv/repoweave"), Role::Dependency));
        assert!(!f.matches(&rp("github/cwalv/repoweave2"), Role::Dependency));
        assert!(!f.matches(&rp("github/other/repoweave"), Role::Dependency));
    }

    #[test]
    fn regex_selector_matches_pattern() {
        let f = RepoFilter::parse(&[], &["re:^github/cwalv/".into()]).unwrap();
        assert!(f.matches(&rp("github/cwalv/repoweave"), Role::Primary));
        assert!(f.matches(&rp("github/cwalv/other"), Role::Reference));
        assert!(!f.matches(&rp("github/other/repoweave"), Role::Primary));
    }

    #[test]
    fn glob_selector_matches_pattern() {
        let f = RepoFilter::parse(&[], &["glob:github/cwalv/*".into()]).unwrap();
        assert!(f.matches(&rp("github/cwalv/repoweave"), Role::Primary));
        assert!(f.matches(&rp("github/cwalv/foo"), Role::Reference));
        assert!(!f.matches(&rp("github/other/foo"), Role::Primary));
    }

    #[test]
    fn glob_star_does_not_cross_slash() {
        // `literal_separator(true)`: a single `*` matches within one path
        // component only. `**` is required to cross `/`. This mirrors
        // .gitignore / ripgrep / cargo conventions.
        let f = RepoFilter::parse(&[], &["glob:github/*".into()]).unwrap();
        assert!(f.matches(&rp("github/cwalv"), Role::Primary));
        assert!(!f.matches(&rp("github/cwalv/repoweave"), Role::Primary));

        let f = RepoFilter::parse(&[], &["glob:github/**".into()]).unwrap();
        assert!(f.matches(&rp("github/cwalv"), Role::Primary));
        assert!(f.matches(&rp("github/cwalv/repoweave"), Role::Primary));
    }

    #[test]
    fn empty_regex_pattern_after_prefix_errors() {
        let err = RepoFilter::parse(&[], &["re:".into()]).unwrap_err();
        assert!(matches!(err, FilterError::EmptyPattern { kind: "re" }));
    }

    #[test]
    fn empty_glob_pattern_after_prefix_errors() {
        let err = RepoFilter::parse(&[], &["glob:".into()]).unwrap_err();
        assert!(matches!(err, FilterError::EmptyPattern { kind: "glob" }));
    }

    #[test]
    fn invalid_regex_errors() {
        let err = RepoFilter::parse(&[], &["re:[unclosed".into()]).unwrap_err();
        assert!(matches!(err, FilterError::InvalidRegex { .. }));
        // Display should mention the bad pattern.
        let msg = format!("{err}");
        assert!(msg.contains("[unclosed"), "got: {msg}");
    }

    #[test]
    fn invalid_glob_errors() {
        // An unbalanced `[` is a glob compile error.
        let err = RepoFilter::parse(&[], &["glob:[unclosed".into()]).unwrap_err();
        assert!(matches!(err, FilterError::InvalidGlob { .. }));
    }

    #[test]
    fn union_role_or_selector() {
        // --role primary --repo github/external/dep — match either.
        let f = RepoFilter::parse(&["primary".into()], &["github/external/dep".into()]).unwrap();
        // Primary-role repo at arbitrary path passes (role match).
        assert!(f.matches(&rp("github/me/code"), Role::Primary));
        // Path-matched repo with non-primary role passes (selector match).
        assert!(f.matches(&rp("github/external/dep"), Role::Dependency));
        // Other dependency-role repos at other paths do not pass.
        assert!(!f.matches(&rp("github/other/dep"), Role::Dependency));
    }

    #[test]
    fn multiple_selectors_accumulate_as_union() {
        let f = RepoFilter::parse(
            &[],
            &[
                "github/a/exact".into(),
                "re:^lib/".into(),
                "glob:vendor/*/proj".into(),
            ],
        )
        .unwrap();
        assert!(f.matches(&rp("github/a/exact"), Role::Primary));
        assert!(f.matches(&rp("lib/foo"), Role::Dependency));
        assert!(f.matches(&rp("vendor/acme/proj"), Role::Reference));
        assert!(!f.matches(&rp("nothing/matches"), Role::Primary));
    }

    #[test]
    fn role_and_selector_lists_both_empty_means_empty() {
        let f = RepoFilter::parse(&[], &[]).unwrap();
        assert!(f.is_empty());
    }

    #[test]
    fn non_empty_filter_reports_not_empty() {
        let f = RepoFilter::parse(&["primary".into()], &[]).unwrap();
        assert!(!f.is_empty());
        let f = RepoFilter::parse(&[], &["x".into()]).unwrap();
        assert!(!f.is_empty());
    }

    #[test]
    fn role_from_str_unknown_errors() {
        assert!(Role::from_str("Primary").is_ok());
        assert!(Role::from_str("primary").is_ok());
        assert!(Role::from_str("bogus").is_err());
    }
}
