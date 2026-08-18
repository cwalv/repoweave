//! Census of the files rwv writes in its own namespace, and how each is
//! published.
//!
//! The defect this catches: a new state file added to `src/` and written with
//! a bare `std::fs::write`. That call truncates before it writes, so a crash
//! or a kill leaves the file permanently short — and rwv's readers of these
//! files treat an unparseable one as absent, which turns a torn write into a
//! detector that reports nothing rather than one that reports a problem.
//!
//! **Structural pin, and why the behavioural form is unavailable.** The
//! difference between a truncating write and an atomic publish is observable
//! only inside the window between `O_TRUNC` and the last byte landing. Nothing
//! the suite can drive deterministically lands a reader or a crash inside that
//! window, which is the first license `docs/internals/testing.md` states — the
//! same one `tests/checkout_classification_single_read_test.rs` runs on. The
//! consequence *of* a torn file is deterministic and is pinned elsewhere; what
//! cannot be driven is rwv producing one.
//!
//! **Population, derived rather than listed.** The scan reads every `const`
//! declaration in `src/` whose name contains `FILE` and whose value is a
//! string literal in rwv's own namespace (`.rwv-…`, `rwv.…`). Each name it
//! finds must be classified in exactly one of the three sets `state_file.rs`
//! declares — `StateFile::ALL`, `EXCLUSIVE_CREATE`, `OPERATOR_AUTHORED` — and
//! the check is set equality in both directions, so a new state file reddens
//! this until it is classified, and a classified name that loses its constant
//! reddens it too. There is no count here to fall out of date.
//!
//! **What this cannot see**, and therefore what it does not vouch for: a state
//! file whose name is written as a bare literal at its use site instead of a
//! named constant; one whose constant is named without `FILE` in it; and one
//! outside rwv's namespace entirely. It also says nothing about *how* a
//! classified file is written — membership in `StateFile::ALL` is what routes
//! a file through the durable publish, and that routing is the type's job, not
//! this scan's.

use repoweave::state_file::{StateFile, EXCLUSIVE_CREATE, OPERATOR_AUTHORED};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Every `.rs` file under `src/`.
fn source_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir)
            .expect("src/ must be readable")
            .flatten()
        {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    walk(&root, &mut out);
    out.sort();
    out
}

/// One `const …FILE…: &str = "<rwv-namespace>"` declaration found in `src/`.
struct Declared {
    /// The file name the constant spells, e.g. `.rwv-active`.
    value: String,
    /// The constant's identifier, e.g. `ACTIVE_PROJECT_FILE`.
    ident: String,
    /// `file.rs:line`, so a failure names the site.
    site: String,
}

/// Where a file's `#[cfg(test)]` module starts, so the scans below read
/// production code only. Fixtures legitimately write these names directly.
fn test_module_line(text: &str) -> usize {
    text.lines()
        .position(|l| l.trim_start().starts_with("#[cfg(test)]"))
        .unwrap_or(usize::MAX)
}

fn declared_state_file_names() -> Vec<Declared> {
    let mut found = Vec::new();
    for path in source_files() {
        let text = std::fs::read_to_string(&path).expect("source must be readable");
        let test_start = test_module_line(&text);
        for (lineno, line) in text
            .lines()
            .enumerate()
            .take_while(|(n, _)| *n < test_start)
        {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || !trimmed.contains("const ") {
                continue;
            }
            let Some((decl, rest)) = trimmed.split_once('=') else {
                continue;
            };
            if !decl.contains("FILE") {
                continue;
            }
            let Some(value) = rest.split('"').nth(1) else {
                continue;
            };
            let bare = value.strip_prefix('.').unwrap_or(value);
            if bare.starts_with("rwv-") || bare.starts_with("rwv.") {
                let ident = decl
                    .split_whitespace()
                    .skip_while(|w| *w != "const")
                    .nth(1)
                    .unwrap_or_default()
                    .trim_end_matches(':')
                    .to_string();
                found.push(Declared {
                    value: value.to_string(),
                    ident,
                    site: format!(
                        "{}:{}",
                        path.file_name().unwrap().to_string_lossy(),
                        lineno + 1
                    ),
                });
            }
        }
    }
    found
}

#[test]
fn every_declared_state_file_is_classified() {
    let declared = declared_state_file_names();
    assert!(
        declared.len() >= StateFile::ALL.len(),
        "the scan found {} state-file constants, fewer than the {} StateFile \
         variants alone — the walk is not reaching src/, so a green result \
         here would mean nothing",
        declared.len(),
        StateFile::ALL.len()
    );

    let classified: BTreeSet<&str> = StateFile::ALL
        .iter()
        .map(|f| f.file_name())
        .chain(EXCLUSIVE_CREATE)
        .chain(OPERATOR_AUTHORED)
        .collect();

    let unclassified: Vec<&str> = declared
        .iter()
        .filter(|d| !classified.contains(d.value.as_str()))
        .map(|d| d.site.as_str())
        .collect();
    assert!(
        unclassified.is_empty(),
        "these files live in rwv's namespace but are in none of \
         StateFile::ALL, EXCLUSIVE_CREATE or OPERATOR_AUTHORED. A state file \
         rwv writes must be published through StateFile so a crash cannot \
         leave it torn; classify each one: {unclassified:?}"
    );

    let seen: BTreeSet<&str> = declared.iter().map(|d| d.value.as_str()).collect();
    let vanished: Vec<&&str> = classified.difference(&seen).collect();
    assert!(
        vanished.is_empty(),
        "these names are classified in state_file.rs but no constant in src/ \
         declares them any more; the classification is describing a file that \
         is gone: {vanished:?}"
    );
}

/// How far from a `fs::write` call the scan looks for a state-file name. Wide
/// enough to span a `let path = …;` and the write that follows it, narrow
/// enough that an unrelated write later in the same function is not attributed
/// to a name mentioned earlier.
const PROXIMITY_LINES: usize = 12;

/// Declarations from `target` paired with each one's identifier, blanked out
/// where the identifier is ambiguous between two different files —
/// `FILE_NAME` is an associated const on more than one type, so the bare
/// identifier does not say which file it names; keeping it would attribute
/// `Manifest::FILE_NAME` to `rwv.lock`. An ambiguous identifier is dropped
/// and only its literal is searched for.
fn keyed_names<'a>(declared: &'a [Declared], target: &BTreeSet<&str>) -> Vec<(&'a str, &'a str)> {
    let ambiguous: BTreeSet<&str> = declared
        .iter()
        .filter(|d| {
            declared
                .iter()
                .any(|other| other.ident == d.ident && other.value != d.value)
        })
        .map(|d| d.ident.as_str())
        .collect();
    declared
        .iter()
        .filter(|d| target.contains(d.value.as_str()))
        .map(|d| {
            let ident = if ambiguous.contains(d.ident.as_str()) {
                ""
            } else {
                d.ident.as_str()
            };
            (ident, d.value.as_str())
        })
        .collect()
}

/// Production sites where a `std::fs::write` call sits within
/// `PROXIMITY_LINES` of one of `names`' idents or literals — each is
/// publishing that file the truncating way.
///
/// **Measured coverage, not assumed.** This sees a site only where the name
/// and the write sit in the same neighbourhood. Reverting `write_owned_digests`
/// or `select_project` to a bare write reddens it; reverting `write_lock` or
/// `WorkweaveMarker::write` does not, because those reach their path through a
/// parameter and through `path_in` respectively and never spell the name.
fn bare_write_sites(names: &[(&str, &str)]) -> Vec<String> {
    let mut findings = Vec::new();
    for path in source_files() {
        let text = std::fs::read_to_string(&path).expect("source must be readable");
        let test_start = test_module_line(&text);
        let lines: Vec<&str> = text.lines().collect();
        for (n, line) in lines.iter().enumerate() {
            if n >= test_start {
                break;
            }
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || !trimmed.contains("fs::write(") {
                continue;
            }
            let lo = n.saturating_sub(PROXIMITY_LINES);
            let hi = (n + PROXIMITY_LINES).min(lines.len());
            let window = lines[lo..hi].join("\n");
            for (ident, value) in names {
                let names_it = (!ident.is_empty() && window.contains(ident))
                    || window.contains(&format!("\"{value}\""));
                if names_it {
                    findings.push(format!(
                        "{}:{} writes {value} with a bare fs::write",
                        path.file_name().unwrap().to_string_lossy(),
                        n + 1
                    ));
                    break;
                }
            }
        }
    }
    findings
}

/// A production site that both names a state file and calls `std::fs::write`
/// is publishing that file the truncating way — the defect itself, as opposed
/// to the census above, which catches a state file nobody classified. The
/// type gate is what covers `write_lock` and `WorkweaveMarker::write`:
/// `StateFile::publish_in` is the only way to name one of these files without
/// spelling it, and it publishes durably.
#[test]
fn no_production_site_writes_a_named_state_file_with_a_bare_write() {
    let declared = declared_state_file_names();
    let durable: BTreeSet<&str> = StateFile::ALL.iter().map(|f| f.file_name()).collect();
    let names = keyed_names(&declared, &durable);
    assert_eq!(
        names.len(),
        StateFile::ALL.len(),
        "every StateFile variant must have a constant for this scan to key on;          without one its site is invisible here and a green result overstates          what was checked"
    );

    let findings = bare_write_sites(&names);
    assert!(
        findings.is_empty(),
        "publish these through StateFile::publish_in so a crash cannot leave \
         them torn: {findings:#?}"
    );
}

/// `EXCLUSIVE_CREATE`'s own prohibition, not covered by the scan above: it
/// filters to `StateFile::ALL` on purpose, so no name published by exclusive
/// create is visible to it. Every one of them is claimed with
/// `durable_file::create_new`, whose refusal on an occupied path IS the
/// exclusion — for the op record and its lease that exclusion is `acquire_op`,
/// and for the ledger and index claim files it is the claim each takes around
/// its read-modify-write. A bare `fs::write` at any of these names replaces
/// instead of refusing, overwriting whatever peer holds the claim.
///
/// Same proximity-window and `#[cfg(test)]`-boundary limitation as the scan
/// above: a name and a write farther apart than `PROXIMITY_LINES`, or one
/// reached through a variable instead of a literal or a named constant, is
/// invisible to this.
#[test]
fn no_production_site_writes_an_exclusive_create_file_with_a_bare_write() {
    let declared = declared_state_file_names();
    let exclusive: BTreeSet<&str> = EXCLUSIVE_CREATE.into_iter().collect();
    let names = keyed_names(&declared, &exclusive);
    assert_eq!(
        names.len(),
        EXCLUSIVE_CREATE.len(),
        "every EXCLUSIVE_CREATE entry must have a constant for this scan to \
         key on; without one its site is invisible here and a green result \
         overstates what was checked"
    );

    let findings = bare_write_sites(&names);
    assert!(
        findings.is_empty(),
        "publish these with durable_file::create_new so a replacement can't \
         overwrite a peer op's claim: {findings:#?}"
    );
}
