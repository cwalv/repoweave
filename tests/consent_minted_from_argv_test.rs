//! Every consent token is minted inside `cli::dispatch::run`, which takes no
//! arguments, so only argv reaches a mint.
//!
//! The compiler holds the mint to the `cli` module tree (`from_flag` is
//! `pub(in crate::cli)`), and every visibility narrower than `pub` looks alike
//! from an external probe. A `pub` function in that tree taking a flag as a
//! `bool` and minting from it compiles, and hands every caller the consent the
//! token exists to withhold. That is a property of where the mint sits, so
//! this reads the source.
//!
//! Scope: non-comment lines of `src/` outside `#[cfg(test)]` items. The token
//! types are the ones `src/cli/consent.rs` gives a `from_flag`, and a mint is
//! a line spelling `<Token>::from_flag(`. A mint through a `use … as` alias,
//! or with the call split from the type across lines, is invisible here.

mod common;

use common::src_scan::{self, SourceLine};

const CONSENT_FILE: &str = "cli/consent.rs";
const DISPATCH_FILE: &str = "cli/dispatch.rs";

fn token_types(lines: &[SourceLine]) -> Vec<String> {
    let mut types = Vec::new();
    let mut current_impl: Option<String> = None;
    for line in lines.iter().filter(|l| l.file == CONSENT_FILE) {
        if let Some(rest) = line.text.strip_prefix("impl ") {
            current_impl = rest.strip_suffix(" {").map(str::to_owned);
        } else if line.text.contains("fn from_flag(") {
            types.extend(current_impl.clone());
        }
    }
    types
}

fn site_key(line: &SourceLine) -> (String, usize) {
    (line.file.clone(), line.line)
}

#[test]
fn every_consent_mint_sits_in_the_argument_less_run() {
    let lines = src_scan::production_lines();

    let types = token_types(&lines);
    let probe = std::any::type_name::<repoweave::cli::consent::DiscardUncommittedConsent>();
    assert!(
        types.iter().any(|t| probe.ends_with(&format!("::{t}"))),
        "the token scan must find the consent types; found {types:?}"
    );

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

    for token in &types {
        let needle = format!("{token}::from_flag(");
        let mints: Vec<&SourceLine> = lines
            .iter()
            .filter(|l| l.file != CONSENT_FILE && l.text.contains(&needle))
            .collect();
        assert!(
            !mints.is_empty(),
            "{token} has a from_flag but no mint was found; the scan is blind"
        );
        for mint in mints {
            assert!(
                run_body.contains(&site_key(mint)),
                "{token} is minted outside cli::dispatch::run at {}: {}",
                mint.site(),
                mint.text.trim()
            );
        }
    }
}
