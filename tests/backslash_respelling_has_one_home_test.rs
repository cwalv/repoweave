//! A `\` in a path becomes a `/` only in `path_spelling.rs`.
//!
//! On Unix `\` is a character in a filename, so a hand-rolled
//! `.replace('\\', "/")` on a path read back from disk turns `a\b` into
//! `a/b`, the name of some other place. The spellings in `path_spelling.rs`
//! rewrite the separator only where the host writes it as `\`, and
//! `RepoPath::below` takes the names the host's own path library splits.
//!
//! Scope: production lines of `src/`, matched on the call's text. A
//! respelling written another way (`.replace("\\", "/")`, a `split('\\')`)
//! is invisible here.

use std::collections::BTreeMap;

mod common;

use common::src_scan;

const HOME: &str = "path_spelling.rs";

const NEEDLE: &str = r#"replace('\\', "/")"#;

struct Allowed {
    file: &'static str,
    count: usize,
    justification: &'static str,
}

const ALLOWED: &[Allowed] = &[Allowed {
    file: "manifest.rs",
    count: 1,
    justification: "A repo key as an operator types it, not a path read back from \
         disk: a Windows shell writes its separators as `\\`, and no member path \
         holds one, so `/` is its only reading on any host.",
}];

#[test]
fn a_backslash_is_respelled_only_where_the_path_spellings_live() {
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in src_scan::production_lines() {
        if line.file != HOME && line.text.contains(NEEDLE) {
            found.entry(line.file.clone()).or_default().push(format!(
                "{}: {}",
                line.site(),
                line.text.trim()
            ));
        }
    }

    let mut offenders = Vec::new();
    for (file, sites) in &found {
        let allowed = ALLOWED.iter().find(|a| a.file == file);
        let count = allowed.map_or(0, |a| a.count);
        if sites.len() != count {
            let reason = allowed.map_or(String::new(), |a| {
                format!("\n  allowed because: {}", a.justification)
            });
            offenders.push(format!(
                "{file}: {} site(s), {count} allowed{reason}\n    {}",
                sites.len(),
                sites.join("\n    ")
            ));
        }
    }
    for allowed in ALLOWED {
        if !found.contains_key(allowed.file) {
            offenders.push(format!(
                "{}: {} site(s) allowed, none found; drop the entry",
                allowed.file, allowed.count
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "a path read back from disk is spelled through path_spelling \
         (weave_relative, WirePath::of) or named through RepoPath::below:\n{}",
        offenders.join("\n")
    );
}
