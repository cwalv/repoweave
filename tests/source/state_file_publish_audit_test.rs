//! Census of the files rwv writes in its own namespace, and the one way a
//! location for any of them is built.
//!
//! The defect this catches: a new state file added to `src/` and written with
//! a bare `std::fs::write`. That call truncates before it writes, so a crash
//! or a kill leaves the file permanently short — and rwv's readers of these
//! files treat an unparseable one as absent, which turns a torn write into a
//! detector that reports nothing rather than one that reports a problem.
//!
//! **The type carries the write discipline.** A `StatePath` is not
//! `AsRef<Path>`, so no path-taking write accepts one (`state_path_seal_test`
//! pins that by diagnostic code), and every write it does offer is durable.
//! What is left for a scan is the two ways round the type, and this file holds
//! both: a state file nobody gave a `StateFile`, and a location built from a
//! state file's name instead of through `StateFile::in_dir`.
//!
//! **Structural pins, and why the behavioral form is unavailable.** Both are
//! prohibitions over an enumerable population (every declared constant in
//! rwv's namespace; every `.join(` in production source), the second license
//! `docs/internals/testing.md` states. The consequence *of* a torn file is
//! deterministic and is pinned elsewhere; what cannot be driven is a future
//! author reaching for the name.
//!
//! **Population, derived rather than listed.** The census reads every `const`
//! declaration in `src/` whose name contains `FILE` and whose value is a
//! string literal in rwv's own namespace (`.rwv-…`, `rwv.…`). Each name it
//! finds must be classified in `StateFile::ALL` or `OPERATOR_AUTHORED`, and
//! the check is set equality in both directions, so a new state file reddens
//! this until it is classified, and a classified name that loses its constant
//! reddens it too. There is no count here to fall out of date.
//!
//! **What this cannot see**, and therefore what it does not vouch for: a state
//! file whose name is written as a bare literal at its use site instead of a
//! named constant; one whose constant is named without `FILE` in it; and one
//! outside rwv's namespace entirely. The location scan reads production
//! source for a `.join(` whose whole argument is a state file's constant (bare
//! or module-qualified), its literal, or a `StateFile` variant's `file_name()`; a name carried through a
//! binding, a `format!`, or a helper before the join is invisible to it, as is
//! a file opened without a join at all.

use repoweave::state_file::{StateFile, OPERATOR_AUTHORED};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Every scanned file's path (relative to the crate root) mapped to its full
/// text — the shared representation the scans below read, so a fixture can
/// drive them without touching disk.
type Corpus = BTreeMap<String, String>;

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

/// The real `src/` tree, read from disk.
fn real_src_corpus() -> Corpus {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = Corpus::new();
    for path in source_files() {
        let text = std::fs::read_to_string(&path).expect("source must be readable");
        let key = path
            .strip_prefix(manifest_dir)
            .expect("source_files() yields paths under CARGO_MANIFEST_DIR")
            .to_string_lossy()
            .into_owned();
        out.insert(key, text);
    }
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
    /// The type whose `impl` block declares it, for an associated constant.
    /// `FILE_NAME` is declared on `Manifest` and on `LockFile`, so the bare
    /// identifier does not say which file it names.
    owner: Option<String>,
}

/// The type of the nearest `impl` header above line `lineno`, for a constant
/// indented inside it. rustfmt puts an `impl` header at column 0 and an
/// associated item inside it at some indent, which is the whole of the
/// distinction drawn here.
fn enclosing_impl(lines: &[&str], lineno: usize) -> Option<String> {
    if !lines[lineno].starts_with(char::is_whitespace) {
        return None;
    }
    lines[..lineno].iter().rev().find_map(|line| {
        let rest = line.strip_prefix("impl")?;
        let rest = rest.strip_prefix('<').map_or(rest, |generic| {
            generic.split_once('>').map_or(generic, |(_, after)| after)
        });
        let rest = rest.trim_start();
        let rest = rest.split_once(" for ").map_or(rest, |(_, target)| target);
        let end = rest.find(['<', ' ', '{']).unwrap_or(rest.len());
        let name = rest[..end].trim();
        (!name.is_empty()).then(|| name.to_string())
    })
}

/// Where a file's `#[cfg(test)]` module starts, so the scans below read
/// production code only. Fixtures legitimately write these names directly.
///
/// A bare `#[cfg(test)]` gates one item — a function, a `mod name;` stub, a
/// macro invocation — not a module, and does not end the production region:
/// only a `#[cfg(test)]` immediately followed by `mod <name> {` does. A file
/// with no such module (every `#[cfg(test)]` gates a single item) has no
/// boundary at all and is read to its end. A `mod <name> {` separated from
/// its `#[cfg(test)]` by another attribute line is not recognized either —
/// unmeasured against this tree, which has none of that shape.
fn test_module_line(text: &str) -> usize {
    let lines: Vec<&str> = text.lines().collect();
    for (n, line) in lines.iter().enumerate() {
        if line.trim_start() != "#[cfg(test)]" {
            continue;
        }
        let Some(next) = lines.get(n + 1) else {
            continue;
        };
        let after_mod = next
            .trim_start()
            .strip_prefix("mod ")
            .or_else(|| next.trim_start().strip_prefix("pub mod "))
            .or_else(|| next.trim_start().strip_prefix("pub(crate) mod "));
        if after_mod.is_some_and(|rest| rest.trim_end().ends_with('{')) {
            return n;
        }
    }
    usize::MAX
}

fn declared_state_file_names(corpus: &Corpus) -> Vec<Declared> {
    let mut found = Vec::new();
    for (path, text) in corpus {
        let test_start = test_module_line(text);
        let all_lines: Vec<&str> = text.lines().collect();
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
                    site: format!("{path}:{}", lineno + 1),
                    owner: enclosing_impl(&all_lines, lineno),
                });
            }
        }
    }
    found
}

#[test]
fn every_declared_state_file_is_classified() {
    let corpus = real_src_corpus();
    let declared = declared_state_file_names(&corpus);
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
        .chain(OPERATOR_AUTHORED)
        .collect();

    let unclassified: Vec<&str> = declared
        .iter()
        .filter(|d| !classified.contains(d.value.as_str()))
        .map(|d| d.site.as_str())
        .collect();
    assert!(
        unclassified.is_empty(),
        "these files live in rwv's namespace but are in neither \
         StateFile::ALL nor OPERATOR_AUTHORED. A state file rwv writes must \
         have a StateFile, so its only location is a StatePath and a crash \
         cannot leave it torn; classify each one: {unclassified:?}"
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

/// The spellings, in production source, of "the name of a state file" as a
/// path component: each declared constant that names one, qualified by its
/// `impl` type where the bare identifier is shared between files, and the
/// quoted literal of every name.
fn name_spellings(declared: &[Declared]) -> Vec<String> {
    let state_names: BTreeSet<&str> = StateFile::ALL.iter().map(|f| f.file_name()).collect();
    let mut out = Vec::new();
    for d in declared
        .iter()
        .filter(|d| state_names.contains(d.value.as_str()))
    {
        let shared = declared
            .iter()
            .any(|other| other.ident == d.ident && other.value != d.value);
        match (&d.owner, shared) {
            (_, false) => out.push(d.ident.clone()),
            (Some(owner), true) => out.push(format!("{owner}::{}", d.ident)),
            (None, true) => {}
        }
        out.push(format!("\"{}\"", d.value));
    }
    out
}

/// The text between the `(` that opens at `open` and its matching `)`, with
/// whitespace runs collapsed so an argument broken across lines compares equal
/// to one written on a line.
fn call_argument(text: &str, open: usize) -> Option<String> {
    let mut depth = 0usize;
    for (offset, ch) in text[open..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    let inner = &text[open + 1..open + offset];
                    return Some(inner.split_whitespace().collect::<Vec<_>>().join(" "));
                }
            }
            _ => {}
        }
    }
    None
}

/// Production sites whose `.join(` argument is, whole, a state file's name:
/// a location built from the name instead of through `StateFile::in_dir`.
/// `src/state_file.rs` is the one place that builds one and is not read.
fn name_joins(corpus: &Corpus, spellings: &[String]) -> Vec<String> {
    let variant_name = regex::Regex::new(r"^(?:\w+::)*StateFile::\w+\.file_name\(\)$")
        .expect("the pattern is a literal");
    let mut findings = Vec::new();
    for (path, text) in corpus {
        if path.ends_with("src/state_file.rs") {
            continue;
        }
        let production: String = text
            .lines()
            .take(test_module_line(text))
            .map(|line| {
                if line.trim_start().starts_with("//") {
                    ""
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        for (at, _) in production.match_indices(".join(") {
            let Some(argument) = call_argument(&production, at + ".join".len()) else {
                continue;
            };
            let argument = argument
                .trim_start_matches('&')
                .trim_end_matches(',')
                .trim();
            let names_a_state_file = spellings
                .iter()
                .any(|s| argument == s || argument.ends_with(&format!("::{s}")))
                || variant_name.is_match(argument);
            if names_a_state_file {
                let line = production[..at].matches('\n').count() + 1;
                findings.push(format!("{path}:{line} joins {argument}"));
            }
        }
    }
    findings
}

/// A location for a state file is built by `StateFile::in_dir` and nowhere
/// else. `StatePath` is only as strong as its being the sole constructor: a
/// writer that joins the name onto a directory holds a `PathBuf`, and every
/// write the type refuses accepts that.
#[test]
fn no_production_code_builds_a_state_file_location_but_state_file() {
    let corpus = real_src_corpus();
    let declared = declared_state_file_names(&corpus);
    let spellings = name_spellings(&declared);
    assert!(
        spellings.len() >= 2 * StateFile::ALL.len() - 1,
        "the scan derived {} spellings from {} state files; it needs a \
         constant and a literal for each, bar the one whose constant name is \
         shared — fewer means it has gone blind, not clean: {spellings:?}",
        spellings.len(),
        StateFile::ALL.len()
    );

    let findings = name_joins(&corpus, &spellings);
    assert!(
        findings.is_empty(),
        "build the location with StateFile::<file>.in_dir(dir) and write \
         through the StatePath it returns: {findings:#?}"
    );
}

fn synthetic(declaration: &str, body: &str) -> Corpus {
    let mut corpus = Corpus::new();
    corpus.insert(
        "src/synthetic_state.rs".to_string(),
        format!("{declaration}\n\n{body}\n"),
    );
    corpus
}

fn joins_in(corpus: &Corpus) -> Vec<String> {
    let declared = declared_state_file_names(corpus);
    name_joins(corpus, &name_spellings(&declared))
}

/// The shape every state-file writer had before `StatePath`: a constant joined
/// onto a directory, then handed to a path-taking write.
#[test]
fn a_state_file_constant_joined_onto_a_directory_is_caught() {
    let corpus = synthetic(
        "pub(crate) const ACTIVE_PROJECT_FILE: &str = \".rwv-active\";",
        "fn publish(dir: &std::path::Path) {\n    std::fs::write(dir.join(ACTIVE_PROJECT_FILE), b\"x\").unwrap();\n}",
    );
    assert_eq!(
        joins_in(&corpus),
        vec!["src/synthetic_state.rs:4 joins ACTIVE_PROJECT_FILE".to_string()]
    );
}

#[test]
fn a_state_file_literal_or_variant_name_joined_onto_a_directory_is_caught() {
    let corpus = synthetic(
        "pub(crate) const ACTIVE_PROJECT_FILE: &str = \".rwv-active\";",
        "fn a(dir: &std::path::Path) -> std::path::PathBuf {\n    dir.join(\".rwv-active\")\n}\nfn b(dir: &std::path::Path) -> std::path::PathBuf {\n    dir.join(StateFile::ProjectLock.file_name())\n}\nfn c(dir: &std::path::Path) -> std::path::PathBuf {\n    dir.join(\n        &ACTIVE_PROJECT_FILE,\n    )\n}\nfn d(dir: &std::path::Path) -> std::path::PathBuf {\n    dir.join(crate::workspace::ACTIVE_PROJECT_FILE)\n}",
    );
    assert_eq!(
        joins_in(&corpus),
        vec![
            "src/synthetic_state.rs:4 joins \".rwv-active\"".to_string(),
            "src/synthetic_state.rs:7 joins StateFile::ProjectLock.file_name()".to_string(),
            "src/synthetic_state.rs:10 joins ACTIVE_PROJECT_FILE".to_string(),
            "src/synthetic_state.rs:15 joins crate::workspace::ACTIVE_PROJECT_FILE".to_string(),
        ]
    );
}

/// `FILE_NAME` is declared on `Manifest` and on `LockFile`; only the second
/// is a state file, and the scan has to tell them apart by their `impl`.
#[test]
fn a_shared_constant_name_is_told_apart_by_its_impl() {
    let corpus = synthetic(
        "impl Manifest {\n    pub const FILE_NAME: &'static str = \"rwv.toml\";\n}\nimpl LockFile {\n    pub const FILE_NAME: &'static str = \"rwv.lock\";\n}",
        "fn a(dir: &std::path::Path) -> std::path::PathBuf {\n    dir.join(Manifest::FILE_NAME)\n}\nfn b(dir: &std::path::Path) -> std::path::PathBuf {\n    dir.join(LockFile::FILE_NAME)\n}",
    );
    assert_eq!(
        joins_in(&corpus),
        vec!["src/synthetic_state.rs:12 joins LockFile::FILE_NAME".to_string()]
    );
}

/// The location built the one sanctioned way, a comment quoting the banned
/// shape, and a test module writing a fixture directly: none is a production
/// use, so none is reported.
#[test]
fn the_sanctioned_construction_and_non_production_text_are_not_reported() {
    let corpus = synthetic(
        "pub(crate) const ACTIVE_PROJECT_FILE: &str = \".rwv-active\";",
        "fn publish(dir: &std::path::Path) {\n    StateFile::ActiveProject.in_dir(dir).publish(b\"x\").unwrap();\n    // dir.join(ACTIVE_PROJECT_FILE) is what this replaced\n}\n#[cfg(test)]\nmod tests {\n    fn fixture(dir: &std::path::Path) {\n        std::fs::write(dir.join(ACTIVE_PROJECT_FILE), \"x\").unwrap();\n    }\n}",
    );
    assert_eq!(joins_in(&corpus), Vec::<String>::new());
}
