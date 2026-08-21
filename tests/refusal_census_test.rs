//! Every refusal site in `src/`, and which instrument reads the token it
//! prints.
//!
//! **The question this asks is not "is this site classified".** A census that
//! only classified would pass over the failure it exists to find: a site that
//! carries a perfectly valid token, and the wrong one. Two of those shipped in
//! this register's own construction — a dominance rule between two tokens that
//! both had entries, and a three-way defect mapping where swapping two arms was
//! invisible to every test binary. In each case the site was classified, the
//! token existed, the entry existed, and no instrument read *that site*.
//!
//! So the manifest carries a third column: the test that drives this site and
//! asserts the token it produced. Where that column is `-`, nothing would
//! notice if the site started emitting a different valid token. **That is the
//! census's finding, not its failure** — the number is large, it is recorded
//! rather than hidden, and it moves only when someone changes it.
//!
//! Structural, under docs/internals/testing.md's licence 2 — a prohibition over
//! an enumerable population.
//!
//! **Scope.** Production lines of `src/` (comment lines and `#[cfg(test)]`
//! items dropped), matched on: a `RefusalKind` mentioned within three lines of
//! a `refuse!` / `refusal(` / `refusing(`; and `bail!` / `anyhow!` macro
//! invocations. `src/bin/generate-explain.rs` is excluded — its errors are
//! developer-facing, not operator-facing.
//!
//! **Residue, stated.** A refusal minted through a mechanism outside those
//! shapes — a new error type printed raw, an `eprintln!` — is invisible here.
//! Typed `Display` variants reach the terminal without a macro site and are
//! counted by their attachment in `src/refusal.rs`, not at the `?` that
//! converts them. The instrument column recognises one shape of read,
//! `assert_routes_to`, so a test that drives a site and checks its token some
//! other way reads as `-` and understates coverage.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const MANIFEST: &str = include_str!("fixtures/refusal-census.tsv");

fn member() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// Production lines of one file: `(line_number, text)`.
fn production_lines(body: &str) -> Vec<(usize, &str)> {
    let lines: Vec<&str> = body.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim_start().starts_with("#[cfg(test)]") {
            let (mut depth, mut seen) = (0i32, false);
            i += 1;
            while i < lines.len() {
                seen |= lines[i].contains('{');
                depth +=
                    lines[i].matches('{').count() as i32 - lines[i].matches('}').count() as i32;
                if seen && depth <= 0 {
                    break;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if !lines[i].trim_start().starts_with("//") {
            out.push((i + 1, lines[i]));
        }
        i += 1;
    }
    out
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("src/ is readable").flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn kebab(variant: &str) -> String {
    let mut out = String::new();
    for (i, c) in variant.char_indices() {
        if c.is_ascii_uppercase() {
            if i != 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Tokens a test drives a site for and asserts by name.
fn site_read_tokens() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut files = Vec::new();
    rust_files(&member().join("tests"), &mut files);
    files.sort();
    for file in files {
        let Ok(body) = std::fs::read_to_string(&file) else {
            continue;
        };
        let rel = file
            .strip_prefix(member())
            .unwrap_or(&file)
            .to_string_lossy()
            .into_owned();
        let mut rest = body.as_str();
        while let Some(at) = rest.find("assert_routes_to(&stderr, \"") {
            let after = &rest[at + 27..];
            if let Some(end) = after.find('"') {
                out.entry(after[..end].to_owned()).or_insert(rel.clone());
                rest = &after[end..];
            } else {
                break;
            }
        }
    }
    out
}

/// The census as the source says it is: `(file, token-or-disposition,
/// instrument) -> site count`.
fn derive() -> BTreeMap<(String, String, String), usize> {
    let reads = site_read_tokens();
    let mut files = Vec::new();
    rust_files(&member().join("src"), &mut files);
    files.sort();

    let mut out: BTreeMap<(String, String, String), usize> = BTreeMap::new();
    for file in files {
        let rel = file
            .strip_prefix(member())
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        if rel.ends_with("generate-explain.rs") {
            continue;
        }
        let body = std::fs::read_to_string(&file).expect("a source file is readable");
        let lines = production_lines(&body);
        for (idx, (_, text)) in lines.iter().enumerate() {
            let window: String = lines[idx.saturating_sub(2)..=idx]
                .iter()
                .map(|(_, t)| *t)
                .collect::<Vec<_>>()
                .join(" ");
            if let Some(variant) = refusal_kind_on(text) {
                if window.contains("refuse!")
                    || window.contains("refusal(")
                    || window.contains("refusing(")
                {
                    let token = kebab(&variant);
                    let instrument = reads.get(&token).cloned().unwrap_or_else(|| "-".into());
                    *out.entry((rel.clone(), token, instrument)).or_default() += 1;
                    continue;
                }
            }
            if is_macro_site(text) {
                *out.entry((rel.clone(), "(untokened)".into(), "-".into()))
                    .or_default() += 1;
            }
        }
    }
    out
}

fn refusal_kind_on(text: &str) -> Option<String> {
    let at = text.find("RefusalKind::")?;
    let rest = &text[at + 13..];
    let end = rest
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(rest.len());
    (end > 0).then(|| rest[..end].to_owned())
}

fn is_macro_site(text: &str) -> bool {
    if text.contains("anyhow::bail!(") || text.contains("anyhow::anyhow!(") {
        return true;
    }
    match text.find("bail!(") {
        Some(0) => true,
        Some(at) => {
            let prev = text.as_bytes()[at - 1];
            !(prev.is_ascii_alphanumeric() || prev == b'_' || prev == b':')
        }
        None => false,
    }
}

/// The manifest, collapsed the same way [`derive`] collapses the source: the
/// untokened dispositions are the manifest's judgment and are not re-derivable,
/// so they compare as one bucket per file.
fn manifest() -> BTreeMap<(String, String, String), usize> {
    let mut out: BTreeMap<(String, String, String), usize> = BTreeMap::new();
    for line in MANIFEST.lines().filter(|l| !l.starts_with('#')) {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() != 4 {
            continue;
        }
        let token = if cols[1].starts_with("(untokened") {
            "(untokened)".to_owned()
        } else {
            cols[1].to_owned()
        };
        *out.entry((cols[0].to_owned(), token, cols[2].to_owned()))
            .or_default() += cols[3].parse::<usize>().expect("a site count");
    }
    out
}

#[test]
fn the_census_matches_the_source() {
    let derived = derive();
    let recorded = manifest();

    let sites: usize = derived.values().sum();
    assert!(
        sites >= 180,
        "the census walk found {sites} sites; it has stopped reading src/"
    );
    assert!(
        derived.len() >= 90,
        "the census walk found {} distinct rows; it has stopped reading src/",
        derived.len()
    );

    let mut drift: Vec<String> = Vec::new();
    for (key, n) in &derived {
        match recorded.get(key) {
            Some(m) if m == n => {}
            Some(m) => drift.push(format!("{key:?}: source has {n}, manifest has {m}")),
            None => drift.push(format!(
                "{key:?}: {n} site(s) in source, absent from manifest"
            )),
        }
    }
    for key in recorded.keys() {
        if !derived.contains_key(key) {
            drift.push(format!("{key:?}: in manifest, no longer in source"));
        }
    }
    assert!(
        drift.is_empty(),
        "the refusal census has drifted. Re-classify each site below and update \
         tests/fixtures/refusal-census.tsv — a new refusal site is not classified \
         until someone decides what reads it:\n{}",
        drift.join("\n")
    );
}

/// The finding, asserted so it cannot grow quietly.
///
/// A site whose instrument column is `-` is one no test drives while checking
/// the token it emits. The count is deliberately exact: adding a refusal site
/// without an instrument moves it, and so does adding an instrument.
#[test]
fn the_unread_population_is_the_recorded_one() {
    let derived = derive();
    let unread: usize = derived
        .iter()
        .filter(|((_, token, instrument), _)| token != "(untokened)" && instrument == "-")
        .map(|(_, n)| *n)
        .sum();
    let read: usize = derived
        .iter()
        .filter(|((_, token, instrument), _)| token != "(untokened)" && instrument != "-")
        .map(|(_, n)| *n)
        .sum();

    assert!(
        read >= 10,
        "only {read} tokened sites have an instrument; the read-detection has stopped working"
    );
    assert_eq!(
        (read, unread),
        (24, 139),
        "the read/unread split of tokened refusal sites has moved. Up is good and \
         down is a regression, but either way the number is a decision: record it."
    );
}

/// Sites nobody has dispositioned yet, held at exactly the known set.
#[test]
fn no_new_site_arrives_unclassified() {
    let unclassified: usize = MANIFEST
        .lines()
        .filter(|l| l.contains("UNCLASSIFIED"))
        .filter_map(|l| l.split('\t').nth(3)?.trim().parse::<usize>().ok())
        .sum();
    assert_eq!(
        unclassified, 4,
        "the number of refusal sites with no recorded disposition changed. Four are \
         known and named in tests/fixtures/refusal-census.tsv; a fifth means a site \
         arrived without anyone deciding what class it is in."
    );
}
