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
}

/// Where a file's `#[cfg(test)]` module starts, so the scans below read
/// production code only. Fixtures legitimately write these names directly.
///
/// A bare `#[cfg(test)]` gates one item — a function, a `mod name;` stub, a
/// macro invocation — not a module, and does not end the production region:
/// only a `#[cfg(test)]` immediately followed by `mod <name> {` does. A file
/// with no such module (every `#[cfg(test)]` gates a single item) has no
/// boundary at all and is read to its end. A `mod <name> {` separated from
/// its `#[cfg(test)]` by another attribute line is not recognised either —
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
///
/// **One write shape only.** The candidate site is a literal `fs::write(` —
/// a `std::fs::OpenOptions` open plus `write_all` publishes without that
/// substring anywhere and is invisible to this, whatever else is true about
/// the name nearby. Production has two such sites today,
/// `durable_file::create_new` and `workweave_index::append_ignore_line`;
/// neither is a violation (the first is the exclusive-create primitive
/// itself, the second targets `.gitignore`/`.git/info/exclude`, not a
/// classified state file), but the shape now exists in this tree and a
/// future writer using it to reach a state file would not be caught here.
fn bare_write_sites(corpus: &Corpus, names: &[(&str, &str)]) -> Vec<String> {
    let mut findings = Vec::new();
    for (path, text) in corpus {
        let test_start = test_module_line(text);
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
                    findings.push(format!("{path}:{} writes {value} with a bare fs::write", n + 1));
                    break;
                }
            }
        }
    }
    findings
}

/// One production function, found by declaration order rather than brace
/// depth: a line belongs to the most recently declared `fn` until the next
/// `impl` or `fn` line updates it. Good enough to say "this write's
/// enclosing function names that accessor somewhere in its body" — not to
/// bound a function's extent exactly.
struct ProductionFn {
    /// `Type::method` for an associated function, bare `name` for a free
    /// one — qualified the way an external caller spells it, since that is
    /// the spelling `writes_reaching_through_accessor` searches for.
    qualified: String,
    /// `fn`/`pub fn`/… declaration line, unparsed — checked for a `PathBuf`
    /// return type by `path_accessor_aliases`.
    signature: String,
    /// Every non-comment line after the signature up to the next `impl` or
    /// `fn` line.
    body: String,
}

/// The `impl Type` (or `impl Trait for Type`) enclosing `trimmed`.
fn impl_type(trimmed: &str) -> Option<&str> {
    let rest = trimmed.strip_prefix("impl ")?;
    let rest = match rest.split_once(" for ") {
        Some((_, target)) => target,
        None => rest,
    };
    let end = rest.find(['<', ' ', '{']).unwrap_or(rest.len());
    let name = rest[..end].trim();
    (!name.is_empty()).then_some(name)
}

/// The name a `fn`/`pub fn`/`pub(crate) fn`/… declaration line introduces.
fn fn_name(trimmed: &str) -> Option<&str> {
    if trimmed.starts_with("//") {
        return None;
    }
    let idx = trimmed.find("fn ")?;
    let before = trimmed[..idx].trim_end();
    let before_is_modifiers = before.is_empty()
        || before
            .split_whitespace()
            .all(|w| matches!(w, "pub" | "async" | "unsafe" | "const") || w.starts_with("pub("));
    if !before_is_modifiers {
        return None;
    }
    let after = &trimmed[idx + "fn ".len()..];
    let end = after.find(['(', '<', ' ', ':']).unwrap_or(after.len());
    let name = &after[..end];
    (!name.is_empty()).then_some(name)
}

/// Every production function in `text`, in declaration order.
fn scan_functions(text: &str, test_start: usize) -> Vec<ProductionFn> {
    let mut out: Vec<ProductionFn> = Vec::new();
    let mut cur_impl: Option<&str> = None;
    for (n, line) in text.lines().enumerate() {
        if n >= test_start {
            break;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        if let Some(t) = impl_type(trimmed) {
            cur_impl = Some(t);
            continue;
        }
        if let Some(name) = fn_name(trimmed) {
            // `cur_impl` never clears at an impl block's closing brace (no
            // depth tracking), so a free function declared right after one
            // would otherwise inherit it. Indentation is the signal instead:
            // this crate's fmt puts every impl member at some indent and
            // every free function at column 0.
            let indented = line.starts_with(' ') || line.starts_with('\t');
            let qualified = match (indented, cur_impl) {
                (true, Some(ty)) => format!("{ty}::{name}"),
                _ => name.to_string(),
            };
            out.push(ProductionFn {
                qualified,
                signature: line.to_string(),
                body: String::new(),
            });
            continue;
        }
        if let Some(last) = out.last_mut() {
            last.body.push_str(line);
            last.body.push('\n');
        }
    }
    out
}

/// Production functions shaped like a path accessor (`-> PathBuf`) whose own
/// body names one of `names` — not by proximity to a call, but because the
/// function IS the accessor. Qualified names (`LeaseRecord::path_in`) are
/// what `writes_reaching_through_accessor` searches a candidate writer's
/// body for, which is what lets a caller that only calls the accessor, and
/// never spells the state file's own name, still be found: the historical
/// shape this exists for is exactly that, a bare write beside
/// `LeaseRecord::path_in(workspace_dir)` and nowhere near `OP_LEASE_FILE`
/// itself.
fn path_accessor_aliases(corpus: &Corpus, names: &[(&str, &str)]) -> BTreeSet<String> {
    let mut aliases = BTreeSet::new();
    for text in corpus.values() {
        let test_start = test_module_line(text);
        for f in scan_functions(text, test_start) {
            if !f.signature.contains("-> PathBuf") && !f.signature.contains("-> std::path::PathBuf")
            {
                continue;
            }
            for (ident, value) in names {
                let hit = (!ident.is_empty() && f.body.contains(ident))
                    || f.body.contains(&format!("\"{value}\""));
                if hit {
                    aliases.insert(f.qualified.clone());
                }
            }
        }
    }
    aliases
}

/// Production `std::fs::write` call sites whose enclosing function's body
/// calls one of `aliases` — reaching one of `EXCLUSIVE_CREATE`'s files
/// through its path accessor instead of spelling the file's own name.
///
/// Same `fs::write(` literal as `bare_write_sites` keys on, and the same gap:
/// a function reaching one of `aliases` through a `std::fs::OpenOptions` open
/// instead is invisible here too.
fn writes_reaching_through_accessor(corpus: &Corpus, aliases: &BTreeSet<String>) -> Vec<String> {
    let mut findings = Vec::new();
    for (path, text) in corpus {
        let test_start = test_module_line(text);
        for f in scan_functions(text, test_start) {
            if !f.body.contains("fs::write(") {
                continue;
            }
            if let Some(alias) = aliases.iter().find(|a| f.body.contains(a.as_str())) {
                findings.push(format!("{path}: {} writes through {alias}", f.qualified));
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
    let corpus = real_src_corpus();
    let declared = declared_state_file_names(&corpus);
    let durable: BTreeSet<&str> = StateFile::ALL.iter().map(|f| f.file_name()).collect();
    let names = keyed_names(&declared, &durable);
    assert_eq!(
        names.len(),
        StateFile::ALL.len(),
        "every StateFile variant must have a constant for this scan to key on;          without one its site is invisible here and a green result overstates          what was checked"
    );

    let findings = bare_write_sites(&corpus, &names);
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
/// above: a name and a write farther apart than `PROXIMITY_LINES` is
/// invisible to this. A write reached through a path-accessor variable
/// (`LeaseRecord::path_in`, `claim_path`) rather than a literal or a named
/// constant is the population
/// `no_production_function_reaches_an_exclusive_create_file_through_its_path_accessor`,
/// below, covers instead.
#[test]
fn no_production_site_writes_an_exclusive_create_file_with_a_bare_write() {
    let corpus = real_src_corpus();
    let declared = declared_state_file_names(&corpus);
    let exclusive: BTreeSet<&str> = EXCLUSIVE_CREATE.into_iter().collect();
    let names = keyed_names(&declared, &exclusive);
    assert_eq!(
        names.len(),
        EXCLUSIVE_CREATE.len(),
        "every EXCLUSIVE_CREATE entry must have a constant for this scan to \
         key on; without one its site is invisible here and a green result \
         overstates what was checked"
    );

    let findings = bare_write_sites(&corpus, &names);
    assert!(
        findings.is_empty(),
        "publish these with durable_file::create_new so a replacement can't \
         overwrite a peer op's claim: {findings:#?}"
    );
}

/// The scan above misses a writer that reaches an `EXCLUSIVE_CREATE` file's
/// path through a helper instead of spelling the file's own name — this is
/// that population, keyed on the accessor's name rather than on proximity to
/// the constant.
///
/// **What this still does not see**: a second hop (an accessor that calls
/// another accessor, rather than joining the constant itself); an accessor
/// not shaped `-> PathBuf`; a call spelled through a `use` import
/// (`path_in(dir)`) rather than the qualified form (`LeaseRecord::path_in(dir)`)
/// this keys on to avoid colliding with `WorkweaveMarker::path_in`, an
/// unrelated accessor for `.rwv-workweave` that shares the bare name; and a
/// write published through `std::fs::OpenOptions` plus `write_all` rather
/// than `std::fs::write` — the shape `bare_write_sites` is also blind to,
/// and one this tree now uses (`workweave_index::append_ignore_line`).
#[test]
fn no_production_function_reaches_an_exclusive_create_file_through_its_path_accessor() {
    let corpus = real_src_corpus();
    let declared = declared_state_file_names(&corpus);
    let exclusive: BTreeSet<&str> = EXCLUSIVE_CREATE.into_iter().collect();
    let names = keyed_names(&declared, &exclusive);
    assert_eq!(
        names.len(),
        EXCLUSIVE_CREATE.len(),
        "every EXCLUSIVE_CREATE entry must have a constant for this scan to \
         key on; without one its accessors are invisible here and a green \
         result overstates what was checked"
    );

    let aliases = path_accessor_aliases(&corpus, &names);
    assert!(
        !aliases.is_empty(),
        "found no PathBuf-returning production function naming an \
         EXCLUSIVE_CREATE file; if the known accessors moved, were renamed, \
         or lost their PathBuf return type, this scan has gone blind, not \
         clean: {names:?}"
    );

    let findings = writes_reaching_through_accessor(&corpus, &aliases);
    assert!(
        findings.is_empty(),
        "these reach an EXCLUSIVE_CREATE file's path through the accessor \
         named, without spelling the file's own name anywhere near the \
         write — publish them with durable_file::create_new instead: \
         {findings:#?}"
    );
}

// ---------------------------------------------------------------------------
// The instrument, driven as its own subject. The two tests above scan the
// real (currently compliant) src/ tree, so a green result there is
// indistinguishable from a scan that finds nothing because it is broken. The
// synthetic corpus below is the shape `write_owned_digests` regressed to when
// this audit last caught it — a declared state-file constant beside a bare
// `fs::write` — so the catch is demonstrated rather than only asserted in
// prose.
// ---------------------------------------------------------------------------

/// A declared state-file constant beside a bare `fs::write` naming it — the
/// exact shape reverting `write_owned_digests` regressed to — must be
/// reported.
#[test]
fn a_bare_write_of_a_named_state_file_is_caught() {
    let mut corpus = Corpus::new();
    corpus.insert(
        "src/synthetic_state.rs".to_string(),
        r#"pub(crate) const WIDGET_FILE: &str = ".rwv-widget";

fn publish(dir: &std::path::Path) {
    let path = dir.join(WIDGET_FILE);
    std::fs::write(path, b"data").unwrap();
}
"#
        .to_string(),
    );

    let declared = declared_state_file_names(&corpus);
    let target: BTreeSet<&str> = [".rwv-widget"].into_iter().collect();
    let names = keyed_names(&declared, &target);
    let findings = bare_write_sites(&corpus, &names);

    assert_eq!(
        findings.len(),
        1,
        "a bare fs::write beside a declared state-file constant must be \
         reported: {findings:?}"
    );
    assert!(
        findings[0].contains(".rwv-widget"),
        "the finding must name the file it caught: {:?}",
        findings[0]
    );
}

/// The same declared constant, published through `StateFile::publish_in`
/// instead of a bare write, must stay quiet — proving the scan discriminates
/// on the write shape rather than merely on proximity to the constant.
#[test]
fn a_publish_in_call_for_the_same_state_file_is_not_reported() {
    let mut corpus = Corpus::new();
    corpus.insert(
        "src/synthetic_state.rs".to_string(),
        r#"pub(crate) const WIDGET_FILE: &str = ".rwv-widget";

fn publish(dir: &std::path::Path, bytes: &[u8]) -> anyhow::Result<()> {
    let path = dir.join(WIDGET_FILE);
    StateFile::Widget.publish_in(dir, bytes)
}
"#
        .to_string(),
    );

    let declared = declared_state_file_names(&corpus);
    let target: BTreeSet<&str> = [".rwv-widget"].into_iter().collect();
    let names = keyed_names(&declared, &target);
    let findings = bare_write_sites(&corpus, &names);

    assert!(
        findings.is_empty(),
        "a publish_in call beside the same declared constant carries no \
         fs::write and must not be reported: {findings:?}"
    );
}
