//! Git is handed a `CloneUrl`, never a manifest `url` as written.
//!
//! A member's `url` keeps the spelling `rwv add` was given, registry
//! shorthand included, and `git clone github/acme/x` fails. Every `Vcs`
//! method that hands git a URL takes a `CloneUrl`, and nothing outside
//! `registry.rs` can make one except by resolving, so a clone site that skips
//! resolution does not compile. Pinned by diagnostic code, from an external
//! crate, where every visibility narrower than `pub` looks alike.
//!
//! Residue: privacy stops at the module that defines the field. Inside
//! `registry.rs` the tuple constructor is reachable, so a mint there that
//! wraps unresolved text compiles.

mod common;

use common::compile_probe::{assert_fails_with, compile};

#[test]
fn the_harness_can_clone_through_a_resolved_url() {
    // Control. Everything below asserts a failure, so a broken rustc
    // invocation would make them all pass for the wrong reason.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::manifest::RepoUrl;
        use repoweave::registry::resolve_to_clone_info;
        use repoweave::vcs::Vcs;
        use std::path::Path;
        pub fn clone(vcs: &dyn Vcs, source: &RepoUrl, dest: &Path) -> bool {
            match resolve_to_clone_info(source) {
                Ok(info) => vcs.clone_repo(&info.url, dest).is_ok(),
                Err(_) => false,
            }
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn clone_repo_does_not_take_text() {
    assert_fails_with(
        "E0308",
        "clone_repo takes a CloneUrl, not a string",
        r#"
        use repoweave::vcs::Vcs;
        use std::path::Path;
        pub fn clone(vcs: &dyn Vcs, dest: &Path) {
            let _ = vcs.clone_repo("x", dest);
        }
        "#,
    );
}

#[test]
fn clone_repo_does_not_take_a_manifest_url() {
    assert_fails_with(
        "E0308",
        "a manifest url reaches clone_repo only through resolution",
        r#"
        use repoweave::manifest::RepoUrl;
        use repoweave::vcs::Vcs;
        use std::path::Path;
        pub fn clone(vcs: &dyn Vcs, source: &RepoUrl, dest: &Path) {
            let _ = vcs.clone_repo(source, dest);
        }
        "#,
    );
}

#[test]
fn clone_attached_at_does_not_take_text() {
    assert_fails_with(
        "E0308",
        "the lock-pinned clone takes a CloneUrl, not a string",
        r#"
        use repoweave::vcs::{LocalRefName, RawRevisionId, Vcs};
        use std::path::Path;
        pub fn clone(vcs: &dyn Vcs, dest: &Path, name: &LocalRefName, at: &RawRevisionId) {
            let _ = vcs.clone_attached_at("x", dest, name, at);
        }
        "#,
    );
}

#[test]
fn add_remote_does_not_take_text() {
    assert_fails_with(
        "E0308",
        "a remote is recorded from a CloneUrl, not a string",
        r#"
        use repoweave::vcs::Vcs;
        use std::path::Path;
        pub fn remote(vcs: &dyn Vcs, repo: &Path) {
            let _ = vcs.add_remote(repo, "x");
        }
        "#,
    );
}

#[test]
fn a_clone_url_cannot_be_converted_from_a_string() {
    assert_fails_with(
        "E0277",
        "CloneUrl has no From<String>",
        r#"
        use repoweave::registry::CloneUrl;
        pub fn mint(text: String) -> CloneUrl {
            text.into()
        }
        "#,
    );
}

#[test]
fn a_clone_url_cannot_be_parsed_from_text() {
    assert_fails_with(
        "E0277",
        "CloneUrl has no FromStr",
        r#"
        use repoweave::registry::CloneUrl;
        pub fn mint(text: &str) -> Option<CloneUrl> {
            text.parse::<CloneUrl>().ok()
        }
        "#,
    );
}

#[test]
fn a_clone_url_cannot_be_built_from_its_field() {
    assert_fails_with(
        "E0423",
        "CloneUrl's field is private to the registry",
        r#"
        use repoweave::registry::CloneUrl;
        pub fn mint(text: String) -> CloneUrl {
            CloneUrl(text)
        }
        "#,
    );
}
