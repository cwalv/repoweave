//! Census of `tests/common/`'s `pub` items and their consumers.
//!
//! The defect this catches: `tests/common/mod.rs` carries
//! `#![allow(dead_code)]`, the standard idiom for a helper module compiled
//! into many test binaries — without it, every binary warns about every
//! helper it does not use. Its cost is that the strongest warning gate,
//! `clippy -D warnings`, cannot see a helper that has gone to zero
//! consumers. A helper can lose its last caller and nothing reports it.
//!
//! STRUCTURAL PIN, licensed as a prohibition over an enumerable population:
//! "has a consumer" is a claim about source, and the enumerable population is
//! every `pub` item under `tests/common/`.
//!
//! **Population, derived rather than listed.** [`parse_items`] reads every
//! tracked `.rs` file under `tests/common/` and matches a top-level `pub fn`
//! / `struct` / `enum` / `trait` / `type` / `const` / `static` declaration. A
//! new file or a new item is picked up on the next run without editing this
//! one; the real pin below asserts a floor on how many it found, so a walk
//! that silently finds fewer than it should fails loudly instead of reporting
//! a clean census.
//!
//! **A consumer is counted three ways**, and an item is reported only when
//! all three come back empty:
//!
//! 1. A qualified reference elsewhere in `tests/` — `common::NAME` for a
//!    `mod.rs` item, `(common::)?MODULE::NAME` for a submodule item.
//! 2. A bare `NAME` in a file that provably imports it by name through a
//!    `use common::…;` statement — the shape every submodule item is
//!    actually called through once brought into scope, since nothing here
//!    calls a submodule item through its qualified path at the call site.
//! 3. A bare `NAME` elsewhere in the item's *own* declaring file — the
//!    module a `pub` item's own sibling functions call it from without any
//!    `use`, which qualified and imported-bare references both miss.
//!
//! Restricting the bare tiers to files that either import the name or
//! declare it is what keeps them from mistaking prose for a call: several
//! items share a name with an ordinary word (`path`, `repo`, `rev`, `sha`,
//! `project`), and an unscoped bare search across all of `tests/` would
//! fabricate consumers out of unrelated identifiers and comments.
//!
//! **What this cannot see**, and therefore does not vouch for: a consumer
//! reached through a re-export or an alias two hops removed from its `use`
//! line; a name quoted in a string literal (counted the same as a real
//! reference, in the rare case one collides); and anything outside `tests/`
//! entirely — this does not read `src/`, because nothing there can `mod
//! common;`.

use regex::{Regex, RegexBuilder};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

/// Every tracked `.rs` file's path (relative to the crate root) mapped to its
/// full text — the shared representation [`parse_items`] and
/// [`zero_consumer_items`] both read, so a fixture can drive them without
/// touching disk.
type Corpus = BTreeMap<String, String>;

struct Item {
    file: String,
    /// `None` for a `tests/common/mod.rs` item, referenced bare as
    /// `common::NAME`; `Some(module)` for a submodule item, referenced as
    /// `module::NAME` or `common::module::NAME`.
    qualifier: Option<String>,
    name: String,
    decl_line: usize,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The real `tests/` tree, read through git's index rather than a directory
/// walk so the corpus matches what is actually tracked.
fn real_tests_corpus() -> Corpus {
    let root = repo_root();
    let listed = Command::new("git")
        .args(["ls-files", "--", "tests"])
        .current_dir(&root)
        .output()
        .expect("git ls-files should run");
    assert!(
        listed.status.success(),
        "git ls-files exited {:?}: {} — a silently failing subprocess reads \
         as an empty corpus, which is exactly the failure this census exists \
         to avoid",
        listed.status.code(),
        String::from_utf8_lossy(&listed.stderr)
    );

    let mut out = Corpus::new();
    for rel in String::from_utf8_lossy(&listed.stdout).lines() {
        if !rel.ends_with(".rs") {
            continue;
        }
        let text =
            std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        out.insert(rel.to_string(), text);
    }
    out
}

/// Every `pub` item declared in a file under `prefix` (a `tests/common/`-style
/// directory prefix, slash-terminated).
fn parse_items(corpus: &Corpus, prefix: &str) -> Vec<Item> {
    let item_re = Regex::new(r"^pub (?:fn|struct|enum|trait|type|const|static)\s+(\w+)").unwrap();
    let mod_rs = format!("{prefix}mod.rs");

    let mut out = Vec::new();
    for (path, text) in corpus {
        if !path.starts_with(prefix) {
            continue;
        }
        let qualifier = if *path == mod_rs {
            None
        } else {
            path.strip_prefix(prefix)
                .and_then(|s| s.strip_suffix(".rs"))
                .map(str::to_string)
        };
        for (i, line) in text.lines().enumerate() {
            if let Some(caps) = item_re.captures(line) {
                out.push(Item {
                    file: path.clone(),
                    qualifier: qualifier.clone(),
                    name: caps[1].to_string(),
                    decl_line: i + 1,
                });
            }
        }
    }
    out
}

/// Every `use common::…;` statement in `corpus`, resolved to the bare names
/// it binds: file → {name → 1-based line of the `use`}. Handles both the
/// single-name (`use common::mod::name;`) and brace (`use
/// common::mod::{a, b as c};`) forms, and `use` statements that wrap across
/// lines.
fn use_common_imports(corpus: &Corpus) -> BTreeMap<String, BTreeMap<String, usize>> {
    let use_re = RegexBuilder::new(r"use\s+common::(.*?);")
        .dot_matches_new_line(true)
        .build()
        .unwrap();
    let brace_re = Regex::new(r"\{([^{}]*)\}\s*$").unwrap();

    let mut out = BTreeMap::new();
    for (path, text) in corpus {
        let mut bound: BTreeMap<String, usize> = BTreeMap::new();
        for caps in use_re.captures_iter(text) {
            let whole = caps.get(0).unwrap();
            let body = &caps[1];
            let import_line = text[..whole.start()].matches('\n').count() + 1;
            let names_part: Vec<&str> = match brace_re.captures(body) {
                Some(bc) => bc
                    .get(1)
                    .unwrap()
                    .as_str()
                    .split(',')
                    .map(str::trim)
                    .collect(),
                None => vec![body.trim()],
            };
            for raw in names_part {
                if raw.is_empty() {
                    continue;
                }
                let bound_name = match raw.split_once(" as ") {
                    Some((_, alias)) => alias.trim(),
                    None => raw.rsplit("::").next().unwrap_or(raw).trim(),
                };
                if !bound_name.is_empty() {
                    bound.insert(bound_name.to_string(), import_line);
                }
            }
        }
        if !bound.is_empty() {
            out.insert(path.clone(), bound);
        }
    }
    out
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

/// Matches of `pattern` across every file in `lines`, skipping
/// `(skip_file, skip_line)` and comment lines.
fn count_matches(
    lines: &BTreeMap<&str, Vec<&str>>,
    pattern: &Regex,
    skip_file: &str,
    skip_line: usize,
) -> usize {
    let mut n = 0;
    for (&path, file_lines) in lines {
        for (i, line) in file_lines.iter().enumerate() {
            let lineno = i + 1;
            if path == skip_file && lineno == skip_line {
                continue;
            }
            if is_comment(line) {
                continue;
            }
            n += pattern.find_iter(line).count();
        }
    }
    n
}

/// Matches of `pattern` within one file's lines, skipping `skip_line` and
/// comment lines.
fn count_in_file(file_lines: &[&str], pattern: &Regex, skip_line: usize) -> usize {
    let mut n = 0;
    for (i, line) in file_lines.iter().enumerate() {
        let lineno = i + 1;
        if lineno == skip_line || is_comment(line) {
            continue;
        }
        n += pattern.find_iter(line).count();
    }
    n
}

/// The items in `items` with no consumer under any of the three tiers this
/// file's header describes.
fn zero_consumer_items<'a>(corpus: &Corpus, items: &'a [Item]) -> Vec<&'a Item> {
    let lines: BTreeMap<&str, Vec<&str>> = corpus
        .iter()
        .map(|(k, v)| (k.as_str(), v.lines().collect()))
        .collect();
    let imports = use_common_imports(corpus);

    let mut zero = Vec::new();
    for item in items {
        let qualified_pattern = match &item.qualifier {
            None => format!(r"\bcommon::{}\b", regex::escape(&item.name)),
            Some(q) => format!(
                r"\b(?:common::)?{}::{}\b",
                regex::escape(q),
                regex::escape(&item.name)
            ),
        };
        let qualified_re = Regex::new(&qualified_pattern).unwrap();
        let qualified = count_matches(&lines, &qualified_re, &item.file, item.decl_line);

        let bare_re = Regex::new(&format!(r"\b{}\b", regex::escape(&item.name))).unwrap();

        let mut imported_bare = 0;
        if qualified == 0 {
            for (path, bound) in &imports {
                if path == &item.file {
                    continue;
                }
                if let Some(&import_line) = bound.get(&item.name) {
                    if let Some(file_lines) = lines.get(path.as_str()) {
                        imported_bare += count_in_file(file_lines, &bare_re, import_line);
                    }
                }
            }
        }

        let mut sibling_bare = 0;
        if qualified == 0 && imported_bare == 0 {
            if let Some(file_lines) = lines.get(item.file.as_str()) {
                sibling_bare = count_in_file(file_lines, &bare_re, item.decl_line);
            }
        }

        if qualified == 0 && imported_bare == 0 && sibling_bare == 0 {
            zero.push(item);
        }
    }
    zero
}

// ---------------------------------------------------------------------------
// The real pin.
// ---------------------------------------------------------------------------

#[test]
fn every_common_pub_item_has_a_consumer() {
    let corpus = real_tests_corpus();
    assert!(
        corpus.len() > 100,
        "the corpus walk yielded {} tracked .rs files under tests/, which is \
         not this repository's test tree — a census that reads nothing \
         reports every helper unused",
        corpus.len()
    );

    let items = parse_items(&corpus, "tests/common/");
    assert!(
        items.len() > 50,
        "the item walk found {} pub items under tests/common/, far fewer than \
         this population has ever held — the declaration pattern likely \
         drifted from the source shape, and the emptiness below would be \
         vacuous",
        items.len()
    );

    let zero = zero_consumer_items(&corpus, &items);
    assert!(
        zero.is_empty(),
        "tests/common/'s #![allow(dead_code)] hides these from clippy — no \
         qualified reference, no bare reference behind a use import, and no \
         reference from a sibling in their own file:\n{}",
        zero.iter()
            .map(|i| format!(
                "  {}:{} {}",
                i.file,
                i.decl_line,
                match &i.qualifier {
                    Some(q) => format!("{q}::{}", i.name),
                    None => i.name.clone(),
                }
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

// ---------------------------------------------------------------------------
// The instrument, driven as its own subject.
// ---------------------------------------------------------------------------

#[test]
fn a_helper_with_no_consumers_anywhere_is_caught() {
    let mut corpus = Corpus::new();
    corpus.insert(
        "tests/common/mod.rs".to_string(),
        "pub fn orphan_helper() {}\n".to_string(),
    );
    corpus.insert(
        "tests/other_test.rs".to_string(),
        "mod common;\nfn f() {}\n".to_string(),
    );

    let items = parse_items(&corpus, "tests/common/");
    let zero = zero_consumer_items(&corpus, &items);

    assert_eq!(
        zero.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
        vec!["orphan_helper"],
        "a pub item with no occurrence anywhere outside its own declaration \
         line must be reported — this is the shape that reached zero \
         consumers and went unnoticed"
    );
}

#[test]
fn a_helper_reached_only_through_a_sibling_in_its_own_file_is_not_reported() {
    let mut corpus = Corpus::new();
    corpus.insert(
        "tests/common/mod.rs".to_string(),
        "pub fn helper() {}\n\npub fn build() {\n    helper();\n}\n".to_string(),
    );

    let items = parse_items(&corpus, "tests/common/");
    let zero = zero_consumer_items(&corpus, &items);

    assert!(
        zero.iter().all(|i| i.name != "helper"),
        "a helper called by a sibling in the same file has a real consumer \
         and must not be reported, even though nothing outside its own file \
         names it"
    );
}

#[test]
fn a_qualified_reference_in_another_file_counts_as_a_consumer() {
    let mut corpus = Corpus::new();
    corpus.insert(
        "tests/common/mod.rs".to_string(),
        "pub fn helper() {}\n".to_string(),
    );
    corpus.insert(
        "tests/other_test.rs".to_string(),
        "mod common;\nfn f() {\n    common::helper();\n}\n".to_string(),
    );

    let items = parse_items(&corpus, "tests/common/");
    let zero = zero_consumer_items(&corpus, &items);

    assert!(
        zero.is_empty(),
        "a qualified call from another file must count as a consumer: {:?}",
        zero.iter().map(|i| i.name.as_str()).collect::<Vec<_>>()
    );
}

#[test]
fn a_bare_reference_behind_a_use_import_counts_as_a_consumer() {
    let mut corpus = Corpus::new();
    corpus.insert(
        "tests/common/doctor_corpus.rs".to_string(),
        "pub fn sha() -> String {\n    String::new()\n}\n".to_string(),
    );
    corpus.insert(
        "tests/other_test.rs".to_string(),
        "mod common;\nuse common::doctor_corpus::sha;\nfn f() {\n    let _ = sha();\n}\n"
            .to_string(),
    );

    let items = parse_items(&corpus, "tests/common/");
    let zero = zero_consumer_items(&corpus, &items);

    assert!(
        zero.is_empty(),
        "a bare call in a file that imports the item by name must count as a \
         consumer — the shape every submodule item in the real corpus is \
         actually called through, since nothing there names a submodule item \
         by its qualified path at the call site: {:?}",
        zero.iter().map(|i| i.name.as_str()).collect::<Vec<_>>()
    );
}

#[test]
fn a_generic_word_in_unrelated_prose_is_not_mistaken_for_a_consumer() {
    let mut corpus = Corpus::new();
    corpus.insert(
        "tests/common/doctor_corpus.rs".to_string(),
        "pub fn repo() -> String {\n    String::new()\n}\n".to_string(),
    );
    corpus.insert(
        "tests/other_test.rs".to_string(),
        "// this test builds a fixture repo and checks its layout\nfn f() {}\n".to_string(),
    );

    let items = parse_items(&corpus, "tests/common/");
    let zero = zero_consumer_items(&corpus, &items);

    assert_eq!(
        zero.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
        vec!["repo"],
        "a file that never imports `repo` by name must not have unrelated \
         prose read as a call to it — the bare tiers are scoped to files \
         that either import the name or declare it"
    );
}
