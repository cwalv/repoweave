//! `registry.rs`'s `placement` is the only site that turns a clone source's
//! identity into a `RepoPath`.
//!
//! Every other `RepoPath::new` call in `src/` takes the operator's raw
//! argument, or a value already observed on disk, directly — never a value
//! computed by mapping a registry, owner and repo through the canonical
//! layout. The compiler cannot enforce this: `RepoPath::new` is a plain
//! constructor and takes whatever `String` a caller hands it, derived or not.
//!
//! **Why this is structural rather than behavioural.** The distinguishing
//! fact is where an argument *came from*, which is a property of the call
//! site's source text, not of any value the program computes at runtime. A
//! second call site re-deriving a path the way `placement` does would in
//! general produce the same string `placement` would have — so no output
//! assertion tells the two apart, and reading the source is what the census
//! habit calls for.
//!
//! **Scope, and therefore the blind spots.** This reads non-comment lines of
//! `src/`, `#[cfg(test)]` items skipped by brace depth (so a fixture quoting
//! the constructor is not read as a live use). It matches the literal
//! `RepoPath::new(` and nothing else, so a caller that renamed the
//! constructor, wrapped it, or split the call across lines is invisible to
//! it by construction. It counts sites per file rather than judging each
//! one's argument, so it catches a *new* or *removed* site — the two changes
//! the invariant actually cares about — without attempting the harder
//! judgment of whether a given argument expression is "derived". That
//! judgment is recorded once, by hand, in each allowlist entry's
//! justification, and re-examined whenever the count it guards moves.

use std::collections::BTreeMap;

mod common;

use common::src_scan::{self, SourceLine};

/// The module that owns `placement`, and therefore the one permitted to
/// construct a `RepoPath` from a resolved identity.
const DERIVED_PRODUCER_MODULE: &str = "registry.rs";

/// The function inside [`DERIVED_PRODUCER_MODULE`] that is the sole derived
/// producer. `placement` itself is a thin `Option`-returning wrapper around
/// this — the construction site is here because the caller that needs to
/// distinguish "nothing to derive from" from "derived a path that fails
/// validation" reads the `Result` this returns.
const DERIVED_PRODUCER_FN: &str = "placement_result";

const NEEDLE: &str = "RepoPath::new(";

/// A file outside [`DERIVED_PRODUCER_MODULE`] permitted to construct a
/// `RepoPath`, and why none of its sites are derived.
struct Allowed {
    file: &'static str,
    count: usize,
    justification: &'static str,
}

const ALLOWLIST: &[Allowed] = &[
    Allowed {
        file: "add_remove.rs",
        count: 4,
        justification: "Two sites are the local-path arm's lookup key — the argument \
             is only reached once it has already been observed as a directory on \
             disk. One reads the origin remote of a clone already on disk (an \
             observed location). One is the removal verb's lookup key, matched \
             against the manifest rather than asserted into it. The creation verb \
             (`run_add_new`) used to hold a fifth site that took the operator's typed \
             path as a layout assertion; it now reads `registry::placement(&plan.url)` \
             instead, so its path is derived from the identity the registry's creation \
             plan mints — inside registry.rs, where a derived path belongs. None of \
             the four remaining sites maps a registry/owner/repo triple through the \
             canonical layout the way `placement` does.",
    },
    Allowed {
        file: "check.rs",
        count: 3,
        justification: "Two build the project repo's own path — not a manifest \
             member, and carrying no registry segment for `placement` to derive from. \
             One is a lookup key compared against the known-repos set built from the \
             manifest. None derives a layout from an identity.",
    },
    Allowed {
        file: "workspace.rs",
        count: 1,
        justification: "The disk scan reports where a checkout already sits — an \
             observed location read off the filesystem, not a path computed from an \
             identity.",
    },
];

/// `file -> [\"file:line: text\", ...]` for every production line outside
/// [`DERIVED_PRODUCER_MODULE`] that spells the needle.
fn sites_by_file(lines: &[SourceLine]) -> BTreeMap<String, Vec<String>> {
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in lines {
        if line.file == DERIVED_PRODUCER_MODULE || !line.text.contains(NEEDLE) {
            continue;
        }
        found.entry(line.file.clone()).or_default().push(format!(
            "{}: {}",
            line.site(),
            line.text.trim()
        ));
    }
    found
}

#[test]
fn no_undeclared_site_outside_registry_constructs_a_repo_path() {
    let lines = src_scan::production_lines();
    let found = sites_by_file(&lines);

    let mut failures = Vec::new();
    for (file, sites) in &found {
        let allowed = ALLOWLIST.iter().find(|a| a.file == file);
        let permitted = allowed.map_or(0, |a| a.count);
        if sites.len() != permitted {
            failures.push(format!(
                "{file}: {} site(s) construct a RepoPath, {permitted} allowed:\n    {}",
                sites.len(),
                sites.join("\n    ")
            ));
        }
    }
    for entry in ALLOWLIST {
        let actual = found.get(entry.file).map_or(0, |s| s.len());
        if actual != entry.count {
            failures.push(format!(
                "{}: allowlist reserves {} site(s) but {actual} remain. A new site \
                 needs its own justification added above; a removed one needs the \
                 count (or the whole entry) dropped rather than left stale.\n  \
                 Recorded reason for the existing entry: {}",
                entry.file, entry.count, entry.justification
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "a RepoPath outside {DERIVED_PRODUCER_MODULE} must be a lookup key or an \
         observed location, never a path derived from an identity — route derivation \
         through `registry::placement` instead.\n\n{}",
        failures.join("\n\n")
    );
}

#[test]
fn registry_rs_is_the_sole_derived_producer() {
    let lines = src_scan::production_lines();

    let total_in_registry = lines
        .iter()
        .filter(|l| l.file == DERIVED_PRODUCER_MODULE && l.text.contains(NEEDLE))
        .count();
    assert_eq!(
        total_in_registry, 1,
        "{DERIVED_PRODUCER_MODULE} should construct exactly one derived RepoPath \
         (inside `{DERIVED_PRODUCER_FN}`); found {total_in_registry}"
    );

    let body = src_scan::body_of(&lines, DERIVED_PRODUCER_MODULE, DERIVED_PRODUCER_FN);
    assert!(
        body.iter().any(|l| l.text.contains(NEEDLE)),
        "the one derived construction site in {DERIVED_PRODUCER_MODULE} must be \
         inside `{DERIVED_PRODUCER_FN}`, not elsewhere in the file"
    );
}

/// A seeded failure, per the "ship one with every check" habit: a fixture the
/// scanner must report, independent of the real `src/` tree's current state.
#[test]
fn an_undeclared_site_is_reported() {
    let fixture = vec![SourceLine {
        file: "hypothetical.rs".to_string(),
        line: 1,
        text: "    let repo_path = RepoPath::new(derived_from_url)?;".to_string(),
    }];

    let found = sites_by_file(&fixture);
    assert!(
        found.contains_key("hypothetical.rs"),
        "a RepoPath::new( call in a file the allowlist has never seen must be \
         reported"
    );
}

/// The scan reaches every file the allowlist and the producer check depend
/// on. A walk that yields nothing is indistinguishable, when green, from one
/// that found nothing — and every file named here carries at least one
/// `RepoPath::new(` today.
#[test]
fn the_scan_reaches_its_known_corpus() {
    let lines = src_scan::production_lines();
    for file in ["add_remove.rs", "check.rs", "workspace.rs", "registry.rs"] {
        assert!(
            lines.iter().any(|l| l.file == file),
            "the scan yielded no production lines for {file}; its walk is broken, \
             not the tree clean"
        );
    }
}
