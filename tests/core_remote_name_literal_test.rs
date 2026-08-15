//! rwv core does not spell the conventional remote's name.
//!
//! The seam's rule is that the backend decides what the remote is called and
//! core reaches it through the methods that act on it, rendering the name —
//! when a message needs one — through `Vcs::conventional_remote_name`. The
//! compiler closes every path that *acts* on a remote: the name's constant is
//! private to `src/git.rs`, and no trait method takes a remote name. It cannot
//! close the sentences that *mention* one, because a string literal is not a
//! path the type system can refuse.
//!
//! **Why this is structural rather than behavioural.** A message that spelled
//! `origin` in core and one that got it from the backend render byte-identical
//! against the git backend, which is the only backend that exists — so no
//! assertion on output can tell them apart. The distinguishing input would be
//! a second backend naming its remote something else, and there is none to
//! drive. That is what licenses reading the source instead.
//!
//! **Scope, and therefore the blind spots.** This reads non-comment lines of
//! `src/`, `#[cfg(test)]` items skipped by brace depth, every file except the
//! backend module — which owns the name and is the point. It matches the
//! standalone word only, so `origin_dir` (rwv's invocation-origin concept, and
//! by far the commonest occurrence in core) and `original` are invisible to it
//! by construction. A `/* … */` block or a trailing `// …` after code is
//! scanned as production text, so a mention parked in one is a finding this
//! reports and a reader has to judge. It sees `src/` and nothing else: a
//! remote name written into `docs/`, a template, or a test fixture is outside
//! it entirely.

use std::collections::BTreeMap;

mod common;

use common::src_scan;

/// The backend module. It owns the remote's name; spelling it there is the
/// whole point of having a seam.
const BACKEND_MODULE: &str = "git.rs";

/// The name the git backend uses. Written here rather than imported because
/// the constant holding it is private to the backend — which is the property
/// this test exists to keep true.
const CONVENTIONAL_REMOTE_NAME: &str = "origin";

/// A site core is allowed to spell the name at, and why.
///
/// Per file, matching how findings are reported, so an exemption cannot widen
/// past the file that earned it.
struct Allowed {
    file: &'static str,
    count: usize,
    justification: &'static str,
}

const ALLOWLIST: &[Allowed] = &[Allowed {
    file: "check.rs",
    count: 1,
    justification: "The doctor finding kind `origin-url-mismatch` is a published wire \
         identifier, carried in docs/reference/schemas/doctor.json and \
         docs/reference/doctor-findings.md. Renaming it is a break for \
         --json consumers, which is a different cost model from a message \
         core happens to phrase — a message is rewritten at will, an \
         identifier consumers match on is not. The render text quoted here \
         echoes the kind, so respelling it alone would leave the finding \
         reading as a different thing from the kind it reports under.",
}];

/// Whether `text` writes the remote's name as a standalone word.
///
/// Boundaries are alphanumeric-or-underscore on either side, so `origin_dir`
/// and `original` do not match and `origin/HEAD` and `origin-url-mismatch` do.
fn spells_the_remote_name(text: &str) -> bool {
    let needle = CONVENTIONAL_REMOTE_NAME;
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(rel) = text[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        let before_ok = start == 0 || !is_word_byte(bytes[start - 1]);
        let after_ok = end == bytes.len() || !is_word_byte(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[test]
fn core_does_not_spell_the_conventional_remote_name() {
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in src_scan::production_lines() {
        if line.file == BACKEND_MODULE || !spells_the_remote_name(&line.text) {
            continue;
        }
        found.entry(line.file.clone()).or_default().push(format!(
            "{}: {}",
            line.site(),
            line.text.trim()
        ));
    }

    let mut failures = Vec::new();
    for (file, sites) in &found {
        let allowed = ALLOWLIST.iter().find(|a| a.file == file);
        let permitted = allowed.map_or(0, |a| a.count);
        if sites.len() > permitted {
            failures.push(format!(
                "{file}: {} site(s) spell the remote name, {permitted} allowed:\n    {}",
                sites.len(),
                sites.join("\n    ")
            ));
        }
    }
    for entry in ALLOWLIST {
        let actual = found.get(entry.file).map_or(0, |s| s.len());
        if actual < entry.count {
            failures.push(format!(
                "{}: allowlist reserves {} site(s) but only {actual} remain — drop the \
                 entry rather than leaving a standing exemption nothing needs.\n  \
                 Recorded reason: {}",
                entry.file, entry.count, entry.justification
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "rwv core must not spell the conventional remote's name. Render it \
         through `Vcs::conventional_remote_name`, or move the sentence behind \
         a trait method that composes it — the way the repair advice for an \
         unset remote default branch is composed.\n\n{}",
        failures.join("\n\n")
    );
}

/// The scan reaches the files it claims to read, and the matcher separates the
/// name from the words that contain it.
///
/// A corpus walk that yields nothing is indistinguishable, when green, from
/// one that found nothing — and every file named here has carried a remote-name
/// mention at some point, so a zero from any of them is the parser breaking
/// rather than the tree being clean.
#[test]
fn the_scan_reaches_its_corpus_and_the_matcher_discriminates() {
    let lines = src_scan::production_lines();
    for file in [
        "add_remove.rs",
        "push.rs",
        "fetch.rs",
        "update.rs",
        "check.rs",
    ] {
        assert!(
            lines.iter().any(|l| l.file == file),
            "the scan yielded no production lines for {file}; its walk is broken, \
             not the tree clean"
        );
    }

    assert!(spells_the_remote_name(
        "run `git remote set-head origin -a` there"
    ));
    assert!(spells_the_remote_name("\"origin-url-mismatch\""));
    assert!(spells_the_remote_name("refs/remotes/origin/HEAD"));
    assert!(!spells_the_remote_name("let ctx = resolve(&origin_dir)?;"));
    assert!(!spells_the_remote_name("carry the original input string"));
}
