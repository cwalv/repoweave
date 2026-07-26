//! Tripwire: every fixture root in `tests/` comes from `common::tempdir()`.
//!
//! `tempfile` hands back whatever `$TMPDIR` names. On macOS that is under
//! `/var`, a symlink to `/private/var`, while rwv canonicalizes the paths it
//! prints and `git worktree list --porcelain` resolves the same way. A test
//! that builds an expected path from a raw temp root is therefore comparing
//! two spellings of one file: green on Linux, red on macOS. That kept macOS
//! CI red from 2026-07-21 through the v0.15.0 release, over seven tests in
//! four targets.
//!
//! `common::tempdir()` canonicalizes the temp root once, so every path
//! derived from it already agrees with what rwv reports. This test is the
//! half that keeps it true: canonicalizing at the *comparison* site is a rule
//! every future test has to remember, and the suite had already accumulated
//! ~99 hand-written `.canonicalize()` calls proving how well that works.
//! Rooted in one constructor, there is no raw temp path in the suite to get
//! wrong — provided nobody reintroduces one.
//!
//! Reproduce the macOS geometry on any platform:
//!
//! ```sh
//! mkdir -p $T/real && ln -s $T/real $T/link
//! TMPDIR=$T/link cargo test --release --no-fail-fast
//! ```
//!
//! Pick a `$T` outside any repoweave weave — a temp root nested under one
//! puts every fixture inside it, and the suite's "outside a workspace" tests
//! then fail for that reason instead, which reads as a much larger blast
//! radius than the defect has.
//!
//! Scope is `tests/` only. `src/`'s unit tests build their fixtures from
//! `tempfile` directly and are clean under the reproduction above: they
//! assert on values rwv computes, not on path strings the test spelled out.

use std::path::{Path, PathBuf};

/// Constructors that hand back a temp dir at whatever path `$TMPDIR` names.
const RAW_CONSTRUCTORS: &[&str] = &["tempfile::tempdir()", "TempDir::new()"];

/// Only this file, which has to quote them in order to look for them.
///
/// `common/mod.rs` is deliberately not exempt. `common::tempdir()` reaches
/// the canonical root through `TempDir::new_in`, so it does not trip the
/// scan today — and a rewrite that reached for either constructor above
/// would defeat the whole fix, which is precisely when this should fire.
const EXEMPT: &[&str] = &["canonical_temp_root_test.rs"];

fn tests_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read tests dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn every_fixture_root_comes_from_the_canonical_constructor() {
    let root = tests_dir();
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    assert!(!files.is_empty(), "no test sources found under {root:?}");

    let mut offenders = Vec::new();
    for file in &files {
        let rel = file
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .to_string();
        if EXEMPT.iter().any(|e| rel.ends_with(e)) {
            continue;
        }
        let text = std::fs::read_to_string(file).expect("readable test source");
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for pattern in RAW_CONSTRUCTORS {
                if line.contains(pattern) {
                    offenders.push(format!("{rel}:{} — {}", n + 1, line.trim()));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "these sites build a fixture root from a raw temp path; use \
         `common::tempdir()` so the root is canonical and expected paths \
         match what rwv prints on macOS:\n{}",
        offenders.join("\n")
    );
}
