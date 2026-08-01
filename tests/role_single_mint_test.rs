//! Pins the role vocabulary (`owned`, `fork`, `dependency`, `reference`) to
//! `Role` in `src/manifest.rs`.
//!
//! selector.rs used to carry its own `impl FromStr for Role` matching all four
//! spellings by hand. A new variant compiled clean and was unparseable via
//! `--role` on fetch/update/push while parseable on `rwv add` — one flag name,
//! two parse paths, and nothing to make them disagree loudly.
//!
//! Three mints exist by construction — serde's `rename_all`, clap's
//! `ValueEnum`, and `Role::as_str` — so the first test here makes them assert
//! against each other rather than trusting that they agree. The second scans
//! `src/` for a fourth appearing, with the spellings taken from `Role::as_str`
//! rather than typed into this file.
//!
//! Residue: the source scan skips `src/bin/`. generate-explain.rs carries an
//! English stopword list containing the word `reference`, which is not a role
//! and no predicate distinguishes it from one. A fourth mint added under
//! `src/bin/` is therefore invisible here.

mod common;

use clap::ValueEnum;
use common::src_scan::{production_lines, SourceLine};
use repoweave::manifest::Role;
use std::str::FromStr;

/// Every `Role`, with a match that stops compiling when a variant is added.
///
/// This is what makes `Role::ALL` checkable: the constant is a hand-written
/// slice, so nothing but this guard notices a variant missing from it.
fn all_roles() -> Vec<Role> {
    let all = vec![Role::Owned, Role::Fork, Role::Dependency, Role::Reference];
    for role in &all {
        match role {
            Role::Owned | Role::Fork | Role::Dependency | Role::Reference => {}
        }
    }
    all
}

#[test]
fn role_all_lists_every_variant() {
    let mut expected = all_roles();
    let mut actual = Role::ALL.to_vec();
    expected.sort_by_key(|r| r.as_str());
    actual.sort_by_key(|r| r.as_str());
    assert_eq!(
        actual, expected,
        "Role::ALL drives --role parsing and its error text; a variant \
         missing from it is a role the CLI cannot name"
    );
}

/// serde, clap and `as_str` are three independent spellings of one vocabulary.
/// Any one of them drifting is what this catches.
#[test]
fn the_three_mints_agree_and_round_trip() {
    let roles = all_roles();
    assert!(!roles.is_empty(), "no variants to check");
    for role in roles {
        let via_as_str = role.as_str();

        let via_serde = match toml::Value::try_from(role).expect("Role serialises") {
            toml::Value::String(s) => s,
            other => panic!("Role must serialise as a string scalar, got {other:?}"),
        };
        assert_eq!(
            via_serde, via_as_str,
            "serde's wire spelling and Role::as_str are one vocabulary"
        );

        let via_clap = role
            .to_possible_value()
            .expect("Role is a clap ValueEnum with no skipped variants");
        assert_eq!(
            via_clap.get_name(),
            via_as_str,
            "clap's --role value and Role::as_str are one vocabulary"
        );

        assert_eq!(
            <Role as FromStr>::from_str(via_as_str).expect("as_str output parses back"),
            role,
            "FromStr must accept what as_str writes"
        );
        assert_eq!(
            <Role as FromStr>::from_str(&via_as_str.to_uppercase())
                .expect("parse is case-insensitive"),
            role
        );
    }
}

/// The legacy spelling must reach the migration hint, not a silent accept and
/// not a bare "unrecognised" message.
///
/// Nothing rewrites a manifest carrying it, so the sentence is the whole
/// remedy: it has to name both the spelling being refused and the one to
/// write instead.
#[test]
fn the_legacy_spelling_is_rejected_with_the_migration_hint() {
    let err = <Role as FromStr>::from_str(Role::LEGACY_SPELLING)
        .expect_err("the legacy spelling must not parse as a role")
        .to_string();
    assert!(
        err.contains(Role::LEGACY_SPELLING) && err.contains(Role::Owned.as_str()),
        "the legacy-spelling rejection must name both the refused spelling \
         and its replacement, got: {err}"
    );
    assert_eq!(
        err,
        Role::legacy_spelling_hint(),
        "manifest loading and --role parsing must reject the legacy spelling \
         with the same sentence; two sentences read as two problems"
    );
}

/// `"<spelling>"` for every variant — the shape a mint takes in source.
fn quoted_spellings() -> Vec<String> {
    all_roles()
        .into_iter()
        .map(|r| format!("\"{}\"", r.as_str()))
        .collect()
}

fn lines_quoting_a_spelling(lines: &[SourceLine], file: &str) -> Vec<SourceLine> {
    let needles = quoted_spellings();
    lines
        .iter()
        .filter(|l| l.file == file)
        .filter(|l| needles.iter().any(|n| l.text.contains(n.as_str())))
        .cloned()
        .collect()
}

#[test]
fn no_module_outside_manifest_spells_the_role_vocabulary() {
    let lines = production_lines();
    let owner = "manifest.rs";

    let minted = lines_quoting_a_spelling(&lines, owner);
    assert!(
        minted.len() >= all_roles().len(),
        "expected every spelling quoted at its mint in src/{owner}, found {} of {} \
         — the needles no longer match the source shape, so emptiness elsewhere \
         would prove nothing. Found: {minted:#?}",
        minted.len(),
        all_roles().len()
    );

    let needles = quoted_spellings();
    let hits: Vec<String> = lines
        .iter()
        .filter(|l| l.file != owner && !l.file.starts_with("bin/"))
        .filter(|l| needles.iter().any(|n| l.text.contains(n.as_str())))
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();
    assert!(
        hits.is_empty(),
        "the role spellings are minted by Role::as_str in src/{owner}; a module \
         that writes one as a literal is a second parse path waiting to \
         disagree. Route it through Role / Role::ALL. Found: {hits:#?}"
    );
}
