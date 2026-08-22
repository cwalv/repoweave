//! Census of the routes by which `tests/` puts bytes into an `rwv.lock`.
//!
//! The defect this catches: a fixture that hand-spells the lock's JSON and
//! writes it as bytes. `common::fixture_lock` exists because such a fixture
//! drifts — to a shape the shipped parser rejects, or to bytes the shipped
//! serializer would not emit — and a lock that fails to parse is read as
//! absent by most of what consumes one, so the drift arrives as a test that
//! still passes while testing less. It had already landed when this census
//! was written: fixtures in `tests/derived_content_adoption_test.rs` and
//! `tests/doc_claims_lock_test.rs` each wrote a TOML-shaped body into a file
//! the code parses as JSON, and the second was the previous lock a test named
//! for overwriting one was overwriting.
//!
//! **Structural pin, licensed as a prohibition over an enumerable
//! population**: the population is every byte-write in `tests/` whose
//! destination is an `rwv.lock` path, and "goes through the shared builder or
//! says why it cannot" is a claim about source.
//!
//! **Population, derived from the write side.** The consolidation that added
//! the builder derived its population from the functions already reaching
//! `LockFile::from_json_str` plus `lock::write_lock` — so a site that skipped
//! both and wrote bytes, the very shape the builder exists to prevent, could
//! not appear in it. [`lock_writes`] instead reads every tracked `.rs` file
//! under `tests/` and reports each call to `fs::write`, `fs::copy`,
//! `fs::rename`, `File::create` or an `OpenOptions` open whose destination
//! argument resolves to a path whose last component is `rwv.lock`. A
//! destination resolves through an inline `.join("…rwv.lock")`, through a bare
//! `"…rwv.lock"` literal, or through an identifier the same file bound to one
//! of those with a `let`. `common::fixture_lock` and `lock::write_lock` are
//! not byte-writes and never appear, which is what makes the report a list of
//! unmediated sites rather than a list of locks.
//!
//! Keying on the destination argument rather than on the line is the
//! precision that makes the report readable: writing `.gitattributes` with
//! `"rwv.lock merge=ours\n"` as the *payload* is how every replay exclusion in
//! this tree is spelled, and a line-keyed matcher reports all of them. Every
//! token the scan keys on is checked against [`code_mask`] first, so source
//! quoted in a string literal — which is what the fixtures at the bottom of
//! this file are — is read as the fixture it is.
//!
//! **A copy or rename whose source is itself an `rwv.lock` path carries no
//! authored bytes** — whatever wrote the source is the site that answers for
//! them — so those are not reported. A copy from anywhere else is.
//!
//! **What a surviving raw site owes.** A `// raw lock bytes: …` line in the
//! comment block leading its own statement group, saying what the shared
//! builder cannot express — non-canonical bytes, a body that must not parse,
//! a payload whose content is irrelevant because the path is the subject. The
//! annotation is per-site because the finding is per-site; a reason that holds
//! for one write of a constant is not thereby a reason for another.
//!
//! **What this cannot see**, and therefore does not vouch for: a write whose
//! destination is a filename held in a variable, which is how a fixture helper
//! taking `(name, contents)` pairs reaches the lock — `lock_writes` sees the
//! helper's generic `fs::write` and no lock path anywhere near it; a path
//! returned by a method rather than bound by a `let`; and anything under
//! `src/`, which does not build fixtures. Bindings are collected per file
//! rather than per function, which would misread a file binding one name to
//! both a lock path and another path —
//! [`no_file_binds_one_name_to_both_a_lock_path_and_another`] is what keeps
//! that from happening silently.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::PathBuf;
use std::process::Command;

/// Every scanned file's path (relative to the crate root) mapped to its full
/// text — the shared representation every scan below reads, so a fixture can
/// drive them without touching disk.
type Corpus = BTreeMap<String, String>;

/// The line a raw lock write must carry, in the comment block leading its own
/// statement group.
const ANNOTATION: &str = "raw lock bytes:";

/// A byte-write whose destination is an `rwv.lock` path.
struct LockWrite {
    file: String,
    /// 1-based line of the write's own verb.
    line: usize,
    verb: &'static str,
    resolution: Resolution,
    annotated: bool,
}

impl LockWrite {
    fn site(&self) -> String {
        format!(
            "{}:{} ({})",
            self.file,
            self.line,
            self.verb.trim_end_matches('(')
        )
    }
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
         as an empty corpus, and an empty corpus reports no unmediated write \
         at all",
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

/// Whether each byte of `text` is code, as opposed to comment or string
/// content.
///
/// A `"` inside a comment must not open a string and a `//` inside a string
/// must not open a comment, or the argument splitting below runs off the end
/// of the expression it is reading. Raw strings carry their own hash count.
/// A `'` opens a character literal only in the two shapes that are one — `'x'`
/// and `'\x'` — so a lifetime does not read as an unterminated literal and
/// `'"'` does not flip the string state.
fn code_mask(text: &str) -> Vec<bool> {
    let b = text.as_bytes();
    let mut mask = vec![false; b.len()];
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(b.len());
            continue;
        }
        if b[i] == b'r' {
            let mut hashes = 0;
            while i + 1 + hashes < b.len() && b[i + 1 + hashes] == b'#' {
                hashes += 1;
            }
            if i + 1 + hashes < b.len() && b[i + 1 + hashes] == b'"' {
                let close = format!("\"{}", "#".repeat(hashes));
                let from = i + 2 + hashes;
                let end = text[from..]
                    .find(&close)
                    .map(|p| from + p + close.len())
                    .unwrap_or(b.len());
                mask[i] = true;
                i = end;
                continue;
            }
        }
        if b[i] == b'"' {
            i += 1;
            while i < b.len() {
                if b[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if b[i] == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if b[i] == b'\'' {
            let one = i + 2 < b.len() && b[i + 2] == b'\'';
            let escaped = i + 3 < b.len() && b[i + 1] == b'\\' && b[i + 3] == b'\'';
            if one || escaped {
                i += if one { 3 } else { 4 };
                continue;
            }
        }
        mask[i] = true;
        i += 1;
    }
    mask
}

/// The byte range of each top-level argument of the call whose `(` is at
/// `open`.
///
/// Ranges rather than substrings, because every test below has to ask the mask
/// whether a token inside an argument is code — this file's own fixtures hold
/// Rust source in string literals, and a scan that reads those as source finds
/// paths this file does not build.
fn call_args(text: &str, mask: &[bool], open: usize) -> Vec<Range<usize>> {
    let b = text.as_bytes();
    let mut depth = 0i32;
    let mut args = Vec::new();
    let mut start = open + 1;
    let mut i = open;
    while i < b.len() {
        if mask[i] {
            match b[i] {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        args.push(start..i);
                        return args;
                    }
                }
                b',' if depth == 1 => {
                    args.push(start..i);
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    args
}

fn last_component_is_lock(literal: &str) -> bool {
    literal.rsplit('/').next() == Some("rwv.lock")
}

/// The file name the path expression in `text[range]` builds: the last string
/// literal it joins.
///
/// A path is assembled a component at a time —
/// `workspace.join("projects").join(name).join("rwv.lock")` — so the earlier
/// literals are directories. Reading them as file names makes one chained
/// binding look like several bindings of the same name.
fn joined_file_name(text: &str, mask: &[bool], range: Range<usize>) -> Option<String> {
    joined_literals(text, mask, range)
        .pop()
        .map(|l| l.rsplit('/').next().unwrap_or(&l).trim().to_string())
}

/// The string literal immediately following each code-position `join(` in
/// `text[range]`.
fn joined_literals(text: &str, mask: &[bool], range: Range<usize>) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = range.start;
    while let Some(hit) = text[at..range.end].find("join(").map(|p| at + p) {
        at = hit + "join(".len();
        if !mask[hit] {
            continue;
        }
        let after = &text[at..range.end];
        let Some(q) = after.find('"') else {
            continue;
        };
        if !after[..q].trim().is_empty() {
            continue;
        }
        if let Some(end) = after[q + 1..].find('"') {
            out.push(after[q + 1..q + 1 + end].to_string());
        }
    }
    out
}

/// An expression stripped of the wrappers a path argument is passed through,
/// leaving a bare identifier where there is one.
fn bare_identifier(expr: &str) -> Option<&str> {
    let mut e = expr.trim();
    e = e.strip_prefix('&').unwrap_or(e).trim();
    for suffix in [".clone()", ".as_path()", ".to_path_buf()"] {
        e = e.strip_suffix(suffix).unwrap_or(e).trim();
    }
    (!e.is_empty() && e.chars().all(|c| c.is_alphanumeric() || c == '_')).then_some(e)
}

/// How a destination expression was recognised as a lock path.
///
/// Recorded rather than discarded because each is a separate piece of
/// parsing: one can break while the others keep working, and a report that
/// still lists the sites the survivors reach is not distinguishable from a
/// whole one. [`every_destination_form_is_exercised`] is what makes it so.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
enum Resolution {
    /// `dir.join("rwv.lock")` written out at the call.
    Inline,
    /// An identifier the file bound to a lock path with a `let`.
    Bound,
    /// A bare `"rwv.lock"` string standing in for the path.
    Literal,
}

/// How the expression in `text[range]` denotes a path whose last component is
/// `rwv.lock`, if it does.
fn lock_path_kind(
    text: &str,
    mask: &[bool],
    range: Range<usize>,
    bound: &BTreeSet<String>,
) -> Option<Resolution> {
    if joined_file_name(text, mask, range.clone()).is_some_and(|n| n == "rwv.lock") {
        return Some(Resolution::Inline);
    }
    let expr = &text[range];
    if let Some(ident) = bare_identifier(expr) {
        if bound.contains(ident) {
            return Some(Resolution::Bound);
        }
    }
    let trimmed = expr.trim();
    if let Some(inner) = trimmed
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| {
            trimmed
                .strip_prefix("std::path::Path::new(\"")
                .and_then(|s| s.strip_suffix("\")"))
        })
    {
        return last_component_is_lock(inner).then_some(Resolution::Literal);
    }
    None
}

fn is_lock_path(text: &str, mask: &[bool], range: Range<usize>, bound: &BTreeSet<String>) -> bool {
    lock_path_kind(text, mask, range, bound).is_some()
}

/// Each identifier this file binds to a joined path with a `let`, mapped to
/// the last components it was bound to.
fn path_bindings(text: &str, mask: &[bool]) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut rest = 0usize;
    while let Some(at) = text[rest..].find("let ") {
        let at = rest + at;
        rest = at + 4;
        if !mask[at] {
            continue;
        }
        let Some(eq) = text[at..].find('=').map(|p| at + p) else {
            continue;
        };
        let Some(semi) = text[eq..].find(';').map(|p| eq + p) else {
            continue;
        };
        let Some(file_name) = joined_file_name(text, mask, eq + 1..semi) else {
            continue;
        };
        let name = text[at + 4..eq]
            .split(':')
            .next()
            .unwrap_or_default()
            .replace("mut ", "");
        let name = name.trim();
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        out.entry(name.to_string()).or_default().insert(file_name);
    }
    out
}

/// Identifiers this file binds to a lock path with a `let`.
fn lock_path_bindings(text: &str, mask: &[bool]) -> BTreeSet<String> {
    path_bindings(text, mask)
        .into_iter()
        .filter(|(_, seen)| seen.contains("rwv.lock"))
        .map(|(name, _)| name)
        .collect()
}

/// Whether the comment block leading `line`'s own statement group carries
/// [`ANNOTATION`].
///
/// The walk climbs through the statement group — a multi-line call, the `let`
/// bindings feeding it — and stops at the first blank line or at a line whose
/// code ends in a brace, so the reason must sit with the write rather than at
/// the top of the enclosing function.
fn annotated(lines: &[&str], mask_lines: &[Vec<bool>], line: usize) -> bool {
    let mut n = line;
    while n > 1 {
        n -= 1;
        let text = lines[n - 1];
        if text.trim().is_empty() {
            return false;
        }
        if text.trim_start().starts_with("//") {
            if text.contains(ANNOTATION) {
                return true;
            }
            continue;
        }
        let last_code = text
            .char_indices()
            .rfind(|(i, c)| {
                !c.is_whitespace() && mask_lines[n - 1].get(*i).copied().unwrap_or(false)
            })
            .map(|(_, c)| c);
        if matches!(last_code, Some('{') | Some('}')) {
            return false;
        }
    }
    false
}

/// Every byte-write in `corpus` whose destination is an `rwv.lock` path.
///
/// A copy or rename out of another `rwv.lock` is left out: it moves bytes some
/// other site authored, and that site is where the reason belongs.
fn lock_writes(corpus: &Corpus) -> Vec<LockWrite> {
    const DEST_AT: &[(&str, usize)] = &[
        ("fs::write(", 0),
        ("File::create(", 0),
        ("fs::copy(", 1),
        ("fs::rename(", 1),
    ];

    let mut out = Vec::new();
    for (path, text) in corpus {
        let mask = code_mask(text);
        let bound = lock_path_bindings(text, &mask);
        let lines: Vec<&str> = text.lines().collect();
        let mask_lines: Vec<Vec<bool>> = {
            let mut v = Vec::with_capacity(lines.len());
            let mut off = 0usize;
            for l in &lines {
                v.push(mask[off..off + l.len()].to_vec());
                off += l.len() + 1;
            }
            v
        };

        let mut sites: Vec<(usize, &'static str, usize)> = Vec::new();
        for (needle, dest_at) in DEST_AT {
            let mut rest = 0usize;
            while let Some(at) = text[rest..].find(needle) {
                let at = rest + at;
                rest = at + needle.len();
                if mask[at] {
                    sites.push((at + needle.len() - 1, needle, *dest_at));
                }
            }
        }
        let mut rest = 0usize;
        while let Some(at) = text[rest..].find("OpenOptions") {
            let at = rest + at;
            rest = at + "OpenOptions".len();
            if !mask[at] {
                continue;
            }
            let stop = text[at..].find(';').map(|p| at + p).unwrap_or(text.len());
            if let Some(open) = text[at..stop].find(".open(").map(|p| at + p) {
                sites.push((open + ".open(".len() - 1, "OpenOptions", 0));
            }
        }

        for (open, verb, dest_at) in sites {
            let args = call_args(text, &mask, open);
            let Some(dest) = args.get(dest_at) else {
                continue;
            };
            let Some(resolution) = lock_path_kind(text, &mask, dest.clone(), &bound) else {
                continue;
            };
            if dest_at == 1
                && args
                    .first()
                    .is_some_and(|src| is_lock_path(text, &mask, src.clone(), &bound))
            {
                continue;
            }
            let line = text[..open].matches('\n').count() + 1;
            out.push(LockWrite {
                file: path.clone(),
                line,
                verb,
                resolution,
                annotated: annotated(&lines, &mask_lines, line),
            });
        }
    }
    out.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    out
}

// ---------------------------------------------------------------------------
// The real pin.
// ---------------------------------------------------------------------------

#[test]
fn every_raw_lock_write_says_why_the_builder_cannot_write_it() {
    let corpus = real_tests_corpus();
    assert!(
        corpus.len() > 100,
        "the corpus walk yielded {} tracked .rs files under tests/, which is \
         not this repository's test tree — a census that reads nothing reports \
         no unmediated write",
        corpus.len()
    );

    let writes = lock_writes(&corpus);
    assert!(
        !writes.is_empty(),
        "the scan found no byte-write to an rwv.lock path anywhere in tests/. \
         This tree has several, each annotated; finding none means the \
         destination parsing broke, not that the tree is clean"
    );

    let unexplained: Vec<String> = writes
        .iter()
        .filter(|w| !w.annotated)
        .map(LockWrite::site)
        .collect();
    assert!(
        unexplained.is_empty(),
        "these put bytes into an rwv.lock without going through \
         common::fixture_lock. Route them through the builder, or state at the \
         site what it cannot express, on a `// {ANNOTATION} …` line:\n{}",
        unexplained.join("\n")
    );
}

/// Every destination form the scan can resolve, that the real tree actually
/// spells, must be found in it.
///
/// `every_raw_lock_write_says_why_the_builder_cannot_write_it` asserts only
/// that the population is non-empty, and that is too weak: the forms are
/// separate pieces of parsing, so one can stop working while the others carry
/// the report — and the sites the survivors still reach are all annotated, so
/// the result is green. That is the "one syntactic form" failure, and this is
/// what makes it red instead.
///
/// [`Resolution::Literal`] is not required. Nothing in this tree writes a lock
/// through a bare relative `"rwv.lock"`; the form is recognised against the
/// day something does, and is exercised only by
/// [`a_destination_spelled_as_a_bare_relative_path_is_reached`].
#[test]
fn every_destination_form_is_exercised() {
    let corpus = real_tests_corpus();
    let found: BTreeSet<Resolution> = lock_writes(&corpus).iter().map(|w| w.resolution).collect();

    for wanted in [Resolution::Inline, Resolution::Bound] {
        assert!(
            found.contains(&wanted),
            "no lock write in tests/ resolved through {wanted:?}, and this tree              has several — that piece of the destination parsing has gone              blind, and the sites the other forms still reach would report a              clean census. Found: {found:?}"
        );
    }
}

/// [`lock_path_bindings`] collects a file's `let`-bound lock paths without
/// tracking which function each belongs to, so a file that spelled one name
/// for both a lock path and another path would have every write to the second
/// read as a write to the first.
///
/// A scope statement saying "measured, and this tree has none" is prose that
/// stops being true without anything noticing. This is the same claim as an
/// assertion.
#[test]
fn no_file_binds_one_name_to_both_a_lock_path_and_another() {
    let corpus = real_tests_corpus();
    let mut lock_binding_files = 0usize;
    let mut collisions = Vec::new();
    for (path, text) in &corpus {
        let mask = code_mask(text);
        let mut names_here = 0usize;
        for (name, seen) in path_bindings(text, &mask) {
            if !seen.contains("rwv.lock") {
                continue;
            }
            names_here += 1;
            if seen.len() > 1 {
                collisions.push(format!("{path}: {name} -> {seen:?}"));
            }
        }
        if names_here > 0 {
            lock_binding_files += 1;
        }
    }

    assert!(
        lock_binding_files > 1,
        "no file was found binding a name to a lock path at all, so this \
         proves nothing about the per-file scope it exists to guard"
    );
    assert!(
        collisions.is_empty(),
        "these bind one name to a lock path and to something else, which the \
         per-file binding scope cannot tell apart — give the lock path its own \
         name, or teach lock_path_bindings the enclosing function:\n{}",
        collisions.join("\n")
    );
}

// ---------------------------------------------------------------------------
// The instrument, driven as its own subject. The pin above scans a tree whose
// every raw write is annotated, so its green is indistinguishable from a scan
// that parses nothing. Each fixture below is one destination shape or one
// discrimination the report depends on.
// ---------------------------------------------------------------------------

fn findings(source: &str) -> Vec<String> {
    let mut corpus = Corpus::new();
    corpus.insert("tests/synthetic_test.rs".to_string(), source.to_string());
    lock_writes(&corpus)
        .iter()
        .filter(|w| !w.annotated)
        .map(LockWrite::site)
        .collect()
}

#[test]
fn a_planted_raw_lock_write_with_no_reason_is_caught() {
    let found = findings(
        r#"fn fixture(dir: &std::path::Path) {
    std::fs::write(dir.join("rwv.lock"), "{}\n").unwrap();
}
"#,
    );
    assert_eq!(
        found.len(),
        1,
        "a bare fs::write into an rwv.lock path is the shape the shared \
         builder exists to replace and must be reported: {found:?}"
    );
    assert!(
        found[0].contains(":2"),
        "the finding names the site: {found:?}"
    );
}

#[test]
fn the_same_write_behind_its_reason_is_not_reported() {
    let found = findings(
        r#"fn fixture(dir: &std::path::Path) {
    // raw lock bytes: trailing whitespace is the only append JSON tolerates.
    std::fs::write(dir.join("rwv.lock"), "{}\n\n").unwrap();
}
"#,
    );
    assert!(
        found.is_empty(),
        "an annotated site states what the builder cannot express and must \
         stay quiet: {found:?}"
    );
}

#[test]
fn a_reason_at_the_top_of_the_function_does_not_cover_a_later_write() {
    let found = findings(
        r#"fn fixture(dir: &std::path::Path) {
    // raw lock bytes: this one is deliberately unparseable.
    std::fs::write(dir.join("rwv.lock"), "not json").unwrap();

    std::fs::write(dir.join("rwv.lock"), "{}\n").unwrap();
}
"#,
    );
    assert_eq!(
        found.len(),
        1,
        "the second write is a separate act with no reason of its own; an \
         annotation that carried across the blank line would exempt writes \
         nobody argued for: {found:?}"
    );
    assert!(
        found[0].contains(":5"),
        "and it is the later one: {found:?}"
    );
}

#[test]
fn a_builder_call_is_not_a_byte_write() {
    let found = findings(
        r#"fn fixture(dir: &std::path::Path) {
    common::fixture_lock(dir, &[("github/acme/server", "https://x", "abc")]);
    repoweave::lock::write_lock(&lock, &dir.join("rwv.lock")).unwrap();
}
"#,
    );
    assert!(
        found.is_empty(),
        "the two mediated routes carry no byte-write verb and must never be \
         reported — a census that flagged them would report correct code: \
         {found:?}"
    );
}

#[test]
fn a_gitattributes_write_naming_the_lock_in_its_payload_is_not_reported() {
    let found = findings(
        r#"fn fixture(dir: &std::path::Path) {
    std::fs::write(dir.join(".gitattributes"), "rwv.lock merge=rwv-ours\n").unwrap();
}
"#,
    );
    assert!(
        found.is_empty(),
        "the literal is the payload, not the destination. This is the \
         discrimination that keeps the report readable: the tree has fifteen \
         of these: {found:?}"
    );
}

#[test]
fn a_write_through_a_local_path_binding_is_reached() {
    let found = findings(
        r#"fn fixture(dir: &std::path::Path) {
    let lock_path = dir.join("rwv.lock");
    let mut body = std::fs::read_to_string(&lock_path).unwrap();
    body.push('\n');
    std::fs::write(&lock_path, body).unwrap();
}
"#,
    );
    assert_eq!(
        found.len(),
        1,
        "binding the destination first is how every read-modify-write in this \
         tree spells it; a matcher keyed only on an inline join misses all of \
         them: {found:?}"
    );
}

/// This file's own fixtures are Rust source held in string literals, so the
/// scan has to tell a `join("rwv.lock")` it is reading from one it is quoting.
/// Getting this wrong is how a census reports itself.
#[test]
fn a_lock_write_quoted_inside_a_string_literal_is_not_a_lock_write() {
    let found = findings(
        r##"fn fixture() {
    let source = r#"std::fs::write(dir.join("rwv.lock"), b);"#;
    drop(source);
}
"##,
    );
    assert!(
        found.is_empty(),
        "quoted source is a fixture, not a write this file performs: {found:?}"
    );
}

#[test]
fn a_destination_spelled_as_a_bare_relative_path_is_reached() {
    let found = findings(
        r#"fn fixture() {
    std::fs::write("rwv.lock", "{}\n").unwrap();
}
"#,
    );
    assert_eq!(
        found.len(),
        1,
        "a relative destination is a lock write too; nothing in the real tree          spells one, so this fixture is the only thing holding the form:          {found:?}"
    );
}

#[test]
fn a_write_spelled_through_file_create_is_reached() {
    let found = findings(
        r#"fn fixture(dir: &std::path::Path) {
    let f = std::fs::File::create(dir.join("rwv.lock")).unwrap();
    drop(f);
}
"#,
    );
    assert_eq!(
        found.len(),
        1,
        "fs::write is one spelling of a byte-write, not the only one: {found:?}"
    );
}

#[test]
fn a_copy_out_of_another_lock_carries_no_authored_bytes() {
    let found = findings(
        r#"fn fixture(from: &std::path::Path, to: &std::path::Path) {
    std::fs::copy(from.join("rwv.lock"), to.join("rwv.lock")).unwrap();
}
"#,
    );
    assert!(
        found.is_empty(),
        "duplicating a lock some other site wrote authors nothing, and the \
         reason belongs at that other site: {found:?}"
    );
}

#[test]
fn a_copy_out_of_anything_else_is_reported() {
    let found = findings(
        r#"fn fixture(from: &std::path::Path, to: &std::path::Path) {
    std::fs::copy(from.join("hand-written.json"), to.join("rwv.lock")).unwrap();
}
"#,
    );
    assert_eq!(
        found.len(),
        1,
        "a copy is a byte-write when its source is not itself a lock — the \
         exemption above turns on the source, not on the verb: {found:?}"
    );
}
