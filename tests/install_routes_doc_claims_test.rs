//! Doc-claim tests anchoring the install routes README.md and
//! docs/introduction.md offer.
//!
//! Doc claims pinned here:
//!
//!   - Neither page offers `cargo install repoweave`: the crate is not
//!     published to crates.io, and no workflow publishes it there, so that
//!     spelling would fail for every reader who tries it.
//!   - Both pages offer the route that actually works instead: `cargo
//!     install --git https://github.com/cwalv/repoweave`.
//!   - Both pages' Homebrew line names the tap `dist-workspace.toml`
//!     actually configures (`brew`'s own convention: a tap repo named
//!     `homebrew-<x>` is addressed as `<owner>/<x>`).
//!
//! Revert either page's `--git` fix, or rename the tap in
//! `dist-workspace.toml` without updating the docs, to watch these go red.

use std::path::Path;

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// `brew install <owner>/<short-name>/<formula>`, derived from
/// `dist-workspace.toml`'s `tap = "<owner>/homebrew-<short-name>"` the same
/// way `brew tap` itself resolves a `homebrew-`-prefixed repo name.
fn expected_brew_install_line() -> String {
    let dist_toml: toml::Value = read("dist-workspace.toml")
        .parse()
        .expect("dist-workspace.toml must parse as TOML");
    let tap = dist_toml["dist"]["tap"]
        .as_str()
        .expect("dist-workspace.toml must set [dist].tap");
    let (owner, repo) = tap
        .split_once('/')
        .unwrap_or_else(|| panic!("dist-workspace.toml's tap '{tap}' must be owner/repo"));
    let short = repo.strip_prefix("homebrew-").unwrap_or_else(|| {
        panic!("tap repo '{repo}' must follow brew's homebrew-<name> convention")
    });
    format!("brew install {owner}/{short}/repoweave")
}

#[test]
fn readme_does_not_offer_cargo_install_from_crates_io() {
    let readme = read("README.md");
    assert!(
        !readme.contains("cargo install repoweave"),
        "README.md must not offer `cargo install repoweave`: the crate is not \
         on crates.io, and no release workflow publishes it there"
    );
    assert!(
        readme.contains("cargo install --git https://github.com/cwalv/repoweave"),
        "README.md must offer the Cargo route that actually works (install \
         from this repo via --git), got:\n{readme}"
    );
}

#[test]
fn introduction_does_not_offer_cargo_install_from_crates_io() {
    let intro = read("docs/introduction.md");
    assert!(
        !intro.contains("cargo install repoweave"),
        "docs/introduction.md must not offer `cargo install repoweave`: the \
         crate is not on crates.io, and no release workflow publishes it there"
    );
    assert!(
        intro.contains("cargo install --git https://github.com/cwalv/repoweave"),
        "docs/introduction.md must offer the Cargo route that actually works \
         (install from this repo via --git), got:\n{intro}"
    );
}

/// No GitHub workflow publishes this crate to crates.io. If one starts to
/// (an added `cargo publish` step, a `crates.io` registry login, or a
/// `CARGO_REGISTRY_TOKEN` secret read), this goes red as the signal to
/// restore `cargo install repoweave` to the docs.
#[test]
fn no_workflow_publishes_to_crates_io() {
    let workflows_dir = repo_root().join(".github/workflows");
    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(&workflows_dir).expect("read .github/workflows") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|s| s.to_str()) != Some("yml") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if text.contains("cargo publish")
            || text.contains("crates.io")
            || text.contains("CARGO_REGISTRY_TOKEN")
        {
            offenders.push(path);
        }
    }
    assert!(
        offenders.is_empty(),
        "a workflow now publishes to crates.io ({offenders:?}) — restore \
         `cargo install repoweave` to README.md and docs/introduction.md"
    );
}

#[test]
fn readme_homebrew_line_names_the_configured_tap() {
    let expected = expected_brew_install_line();
    let readme = read("README.md");
    assert!(
        readme.contains(&expected),
        "README.md's Homebrew line must match dist-workspace.toml's tap \
         ({expected}), got:\n{readme}"
    );
}

#[test]
fn introduction_homebrew_line_names_the_configured_tap() {
    let expected = expected_brew_install_line();
    let intro = read("docs/introduction.md");
    assert!(
        intro.contains(&expected),
        "docs/introduction.md's Homebrew line must match dist-workspace.toml's \
         tap ({expected}), got:\n{intro}"
    );
}
