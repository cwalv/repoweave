//! "Same URL" means "names the same remote", and it is the only comparison.
//!
//! A manifest `url` keeps the operator's spelling, so `github/acme/x` and
//! `https://github.com/acme/x.git` are one remote written two ways. A
//! structural `==` on `RepoUrl` calls them different, and a string comparison
//! of unresolved spellings does the same. Both are gone: two remotes compare
//! only as `CloneUrl`s, after resolution, through `names_same_remote`. Pinned
//! by diagnostic code from an external crate.

mod common;

use common::compile_probe::{assert_fails_with, compile};

#[test]
fn the_harness_can_compare_two_resolved_remotes() {
    // Control. Everything below asserts a failure, so a broken rustc
    // invocation would make them all pass for the wrong reason.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::manifest::RepoUrl;
        use repoweave::registry::resolve_to_clone_info;
        pub fn same(a: &RepoUrl, b: &RepoUrl) -> bool {
            match (resolve_to_clone_info(a), resolve_to_clone_info(b)) {
                (Ok(a), Ok(b)) => a.url.names_same_remote(&b.url),
                _ => false,
            }
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn repo_urls_have_no_structural_equality() {
    assert_fails_with(
        "E0369",
        "RepoUrl == RepoUrl does not compile",
        r#"
        use repoweave::manifest::RepoUrl;
        pub fn same(a: &RepoUrl, b: &RepoUrl) -> bool {
            a == b
        }
        "#,
    );
}

#[test]
fn clone_urls_have_no_structural_equality() {
    assert_fails_with(
        "E0369",
        "CloneUrl == CloneUrl does not compile",
        r#"
        use repoweave::registry::CloneUrl;
        pub fn same(a: &CloneUrl, b: &CloneUrl) -> bool {
            a == b
        }
        "#,
    );
}

#[test]
fn unresolved_spellings_have_no_string_comparison() {
    assert_fails_with(
        "E0432",
        "no comparison takes two URL strings",
        r#"
        use repoweave::manifest::clone_urls_equivalent;
        pub fn same(a: &str, b: &str) -> bool {
            clone_urls_equivalent(a, b)
        }
        "#,
    );
}
