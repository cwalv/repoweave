//! Every consent token is minted inside `cli::dispatch::run`, which takes no
//! arguments, so only argv reaches a mint.
//!
//! The compiler holds the mint to the `cli` module tree (each mint is
//! `pub(in crate::cli)`), and every visibility narrower than `pub` looks alike
//! from an external probe. A `pub` function in that tree taking a flag as a
//! `bool` and minting from it compiles, and hands every caller the consent the
//! token exists to withhold. That is a property of where the mint sits, so
//! this reads the source. A `--continue` mint reads the op record `run`
//! resolved from the workspace it runs in, which no caller hands it either.
//!
//! Scope: non-comment lines of `src/` outside `#[cfg(test)]` items. The mints
//! are the `pub(in crate::cli) fn`s of the `impl` blocks in
//! `src/cli/consent.rs`, and a mint site is a line spelling
//! `<Token>::<mint>(`. A mint through a `use … as` alias, or with the call
//! split from the type across lines, is invisible here.

mod common;

use common::src_scan::{self, SourceLine};

const CONSENT_FILE: &str = "cli/consent.rs";
const DISPATCH_FILE: &str = "cli/dispatch.rs";

const MINT_VISIBILITY: &str = "pub(in crate::cli) fn ";

fn mints(lines: &[SourceLine]) -> Vec<(String, String)> {
    let mut mints = Vec::new();
    let mut current_impl: Option<String> = None;
    for line in lines.iter().filter(|l| l.file == CONSENT_FILE) {
        if let Some(rest) = line.text.strip_prefix("impl ") {
            current_impl = rest.strip_suffix(" {").map(str::to_owned);
        } else if let Some(at) = line.text.find(MINT_VISIBILITY) {
            let name = line.text[at + MINT_VISIBILITY.len()..]
                .split(['(', '<'])
                .next()
                .unwrap_or_default()
                .to_owned();
            if let Some(token) = &current_impl {
                mints.push((token.clone(), name));
            }
        }
    }
    mints
}

fn site_key(line: &SourceLine) -> (String, usize) {
    (line.file.clone(), line.line)
}

#[test]
fn every_consent_mint_sits_in_the_argument_less_run() {
    let lines = src_scan::production_lines();

    let mints = mints(&lines);
    for probe in [
        std::any::type_name::<repoweave::cli::consent::DiscardUncommittedConsent>(),
        std::any::type_name::<repoweave::cli::consent::DiscardLocalCommitsConsent>(),
    ] {
        assert!(
            mints
                .iter()
                .any(|(t, _)| probe.ends_with(&format!("::{t}"))),
            "the mint scan must find {probe}; found {mints:?}"
        );
    }

    let run_signature = lines
        .iter()
        .find(|l| l.file == DISPATCH_FILE && src_scan::top_level_fn_name(&l.text) == Some("run"))
        .expect("cli::dispatch::run is a top-level fn");
    assert!(
        run_signature.text.contains("fn run()"),
        "run must take no arguments, or a caller chooses the flags it mints from: {}",
        run_signature.text
    );

    let run_body: std::collections::BTreeSet<_> = src_scan::body_of(&lines, DISPATCH_FILE, "run")
        .iter()
        .map(site_key)
        .collect();

    for (token, mint) in &mints {
        let needle = format!("{token}::{mint}(");
        let sites: Vec<&SourceLine> = lines
            .iter()
            .filter(|l| l.file != CONSENT_FILE && l.text.contains(&needle))
            .collect();
        assert!(
            !sites.is_empty(),
            "{token}::{mint} has no call site; the scan is blind"
        );
        for site in sites {
            assert!(
                run_body.contains(&site_key(site)),
                "{token} is minted outside cli::dispatch::run at {}: {}",
                site.site(),
                site.text.trim()
            );
        }
    }
}
