//! Pins the role vocabulary (`owned`, `fork`, `dependency`, `reference`) to
//! `Role` in `src/manifest.rs`.
//!
//! A role is spelled by serde's `rename_all`; `Display` reads that spelling
//! back through `wire_token`, `FromStr` matches against `Display`, and clap's
//! `ValueEnum` derives its values from the same kebab-case rule. Clap's rule is
//! declared separately from serde's, so the first test makes the spellings
//! assert against each other. The second scans `src/` for one written as a
//! literal, with the spellings taken from `Display` rather than typed here.
//!
//! Residue: the source scan skips `src/bin/`. generate-explain.rs carries an
//! English stopword list containing the word `reference`, which is not a role
//! and no predicate distinguishes it from one. A fourth mint added under
//! `src/bin/` is therefore invisible here.

mod common;

use clap::ValueEnum;
use common::src_scan::production_lines;
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
    expected.sort();
    actual.sort();
    assert_eq!(
        actual, expected,
        "Role::ALL drives --role parsing and its error text; a variant \
         missing from it is a role the CLI cannot name"
    );
}

/// serde and clap declare their rename rules separately, and `Display` and
/// `FromStr` read serde's. Any one of them drifting is what this catches.
#[test]
fn the_spellings_agree_and_round_trip() {
    let roles = all_roles();
    assert!(!roles.is_empty(), "no variants to check");
    for role in roles {
        let via_display = role.to_string();
        let via_as_str = via_display.as_str();

        let via_serde = match toml::Value::try_from(role).expect("Role serializes") {
            toml::Value::String(s) => s,
            other => panic!("Role must serialize as a string scalar, got {other:?}"),
        };
        assert_eq!(
            via_serde, via_as_str,
            "serde's wire spelling and Role's Display are one vocabulary"
        );

        let via_clap = role
            .to_possible_value()
            .expect("Role is a clap ValueEnum with no skipped variants");
        assert_eq!(
            via_clap.get_name(),
            via_as_str,
            "clap's --role value and Role's Display are one vocabulary"
        );

        assert_eq!(
            <Role as FromStr>::from_str(via_as_str).expect("Display output parses back"),
            role,
            "FromStr must accept what Display writes"
        );
        assert_eq!(
            <Role as FromStr>::from_str(&via_as_str.to_uppercase())
                .expect("parse is case-insensitive"),
            role
        );
    }
}

/// The legacy spelling must reach the migration hint, not a silent accept and
/// not a bare "unrecognized" message.
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
        err.contains(Role::LEGACY_SPELLING) && err.contains(&Role::Owned.to_string()),
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
        .map(|r| format!("\"{r}\""))
        .collect()
}

#[test]
fn no_module_spells_the_role_vocabulary() {
    let needles = quoted_spellings();
    let manifest_source =
        std::fs::read_to_string(common::src_scan::src_dir().join("manifest.rs")).unwrap();
    assert!(
        needles.iter().any(|n| manifest_source.contains(n.as_str())),
        "src/manifest.rs's own tests quote a role in a TOML fixture; none of {needles:?} \
         matches there, so the needles no longer match the source shape and emptiness below \
         would prove nothing"
    );

    let hits: Vec<String> = production_lines()
        .iter()
        .filter(|l| !l.file.starts_with("bin/"))
        .filter(|l| needles.iter().any(|n| l.text.contains(n.as_str())))
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();
    assert!(
        hits.is_empty(),
        "a role is spelled by Role's serde rename rule in src/manifest.rs; a module \
         that writes one as a literal is a second parse path waiting to disagree. \
         Route it through Role / Role::ALL. Found: {hits:#?}"
    );
}
