//! Tripwire: every fixture root a test builds under `$TMPDIR` comes from a
//! canonical constructor — `common::tempdir()` in `tests/`,
//! `crate::tempdir()` in `src/`.
//!
//! `tempfile` hands back whatever `$TMPDIR` names. On macOS that is under
//! `/var`, a symlink to `/private/var`, while rwv resolves the locations it
//! records and prints, and `git worktree list --porcelain` resolves the same
//! way. A test that builds an expected path from a raw temp root is therefore
//! comparing two spellings of one file: green on Linux, red on macOS. That
//! kept macOS CI red from 2026-07-21 through the v0.15.0 release, over seven
//! tests in four targets.
//!
//! Each constructor resolves the temp root once, so every path derived from
//! it already agrees with what rwv reports. This test is the half that keeps
//! it true: canonicalizing at the *comparison* site is a rule every future
//! test has to remember, and the suite had already accumulated ~99
//! hand-written `.canonicalize()` calls proving how well that works. Rooted in
//! a constructor, there is no raw temp path in the suite to get wrong —
//! provided nobody reintroduces one.
//!
//! There is one constructor per crate a test compiles into, because none can
//! see another's: `common` is compiled into each `tests/` target, and a
//! `#[cfg(test)]` item of the library does not exist for a dependent. So
//! `crate::tempdir()` is the one at the root of the crate the test is in —
//! `src/lib.rs` for the library's inline modules, the binary's own root for a
//! binary's.
//!
//! SCOPE. Every `.rs` file under `tests/` and `src/`, read whole: production
//! code cannot reach `tempfile`, a dev-dependency, so reading it needs no
//! test-region boundary. A line is a finding when it holds a constructor in
//! [`RAW_CONSTRUCTORS`], or a `tempdir()` call qualified by anything but its
//! own tree's constructor path. A finding is the construction, whatever the
//! test goes on to assert: a `contains` that lets `/var/…` match inside
//! `/private/var/…` hides the raw root from macOS, not from this scan.
//!
//! RESIDUE. A root named by the caller — `TempDir::new_in(..)`,
//! `tempdir_in(..)`, a directory created at `std::env::temp_dir().join(..)`
//! — is not read. A root that is not `$TMPDIR` is the caller's choice; one
//! that is, spelled this way, is a raw root this scan does not see. The
//! comment filter is line-leading `//` only, so a constructor named in a
//! block or trailing comment reads as a call: a false finding, not a miss.
//!
//! The behavioral half is `scripts/ci-local.sh`, which runs its test stages
//! with `$TMPDIR` behind a symlink. There a raw root reddens any test that
//! compares a path built from it, however the root was spelled, and the link
//! and its target share no substring, so a `contains` does not collapse them.
//! A host that cannot make the symlink runs those stages without it, and says
//! so; this scan reads the source the same on every host.
//!
//! The same geometry for a bare `cargo test`:
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

use std::path::{Path, PathBuf};

/// Constructors that hand back a temp dir at whatever path `$TMPDIR` names.
const RAW_CONSTRUCTORS: &[&str] = &["tempfile::tempdir()", "TempDir::new()"];

/// Only this file, which has to quote them in order to look for them.
///
/// The constructors' own definitions are deliberately not exempt. Each
/// reaches the resolved root through `TempDir::new_in`, so none trips the
/// scan today — and a rewrite that reached for a raw constructor instead
/// would defeat the whole fix, which is precisely when this should fire.
const EXEMPT: &str = "tests/canonical_temp_root_test.rs";

/// The trees read, each with the one qualifier a `tempdir()` call there may
/// carry.
const TREES: &[(&str, &str)] = &[("tests", "common::"), ("src", "crate::")];

/// Whether `line` builds a fixture root from the raw temp root, read in a
/// tree whose constructor is reached through `qualifier`.
///
/// The literals in [`RAW_CONSTRUCTORS`] are written the way the offending
/// call usually reads, but `use tempfile::tempdir;` makes the call site read
/// as a bare `tempdir()` and no literal matches it. So a `tempdir()` call is
/// also read by its qualifier: anything but the tree's own constructor builds
/// the root somewhere this test cannot vouch for. A `fn tempdir()` is a
/// definition, not a call.
fn raw_root_at(line: &str, qualifier: &str) -> bool {
    if RAW_CONSTRUCTORS.iter().any(|p| line.contains(p)) {
        return true;
    }
    let mut rest = line;
    while let Some(i) = rest.find("tempdir()") {
        let before = &rest[..i];
        if !before.ends_with(qualifier) && !before.trim_end().ends_with("fn") {
            return true;
        }
        rest = &rest[i + "tempdir()".len()..];
    }
    false
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read source dir").flatten() {
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
    let package = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();

    for (tree, qualifier) in TREES {
        let accepted = format!("{qualifier}tempdir()");
        let mut files = Vec::new();
        rust_files(&package.join(tree), &mut files);
        let mut accepted_calls = 0usize;

        for file in &files {
            let rel = file
                .strip_prefix(package)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if rel == EXEMPT {
                continue;
            }
            let text = std::fs::read_to_string(file).expect("readable source");
            for (n, line) in text.lines().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                accepted_calls += line.matches(accepted.as_str()).count();
                if raw_root_at(line, qualifier) {
                    offenders.push(format!("{rel}:{} — {}", n + 1, line.trim()));
                }
            }
        }

        assert!(
            accepted_calls > 0,
            "the walk over {tree}/ read {} files and found no `{accepted}` call, \
             so it is not reading the tree and a clean result below is vacuous",
            files.len()
        );
    }

    assert!(
        offenders.is_empty(),
        "these sites build a fixture root from a raw temp path; use \
         `common::tempdir()` under tests/ or `crate::tempdir()` in src/, so the \
         root is resolved and expected paths match what rwv prints on macOS:\n{}",
        offenders.join("\n")
    );
}

/// The predicate reports each raw spelling in each tree, and passes the
/// tree's own constructor and its definition.
#[test]
fn the_scan_reports_a_raw_root_in_either_tree() {
    for (tree, qualifier) in TREES {
        for raw in [
            "let tmp = tempfile::tempdir().unwrap();",
            "let tmp = TempDir::new().unwrap();",
            "let tmp = tempfile::TempDir::new().unwrap();",
            "let tmp = tempdir().unwrap();",
            "let tmp = tempfile::Builder::new().prefix(\"x\").tempdir().unwrap();",
        ] {
            assert!(
                raw_root_at(raw, qualifier),
                "{tree}/: `{raw}` builds a root from the raw temp dir and must be reported"
            );
        }
        assert!(
            !raw_root_at(
                &format!("let tmp = {qualifier}tempdir().unwrap();"),
                qualifier
            ),
            "{tree}/: the tree's own constructor is not a finding"
        );
        assert!(
            !raw_root_at(
                "pub(crate) fn tempdir() -> std::io::Result<tempfile::TempDir> {",
                qualifier
            ),
            "{tree}/: a constructor's definition is not a call"
        );
    }

    assert!(
        raw_root_at("let tmp = crate::tempdir().unwrap();", "common::"),
        "under tests/, `crate::tempdir()` names a constructor the test target \
         defined for itself, which this scan cannot vouch for"
    );
    assert!(
        raw_root_at("let tmp = common::tempdir().unwrap();", "crate::"),
        "in src/, only the crate root's constructor is accepted"
    );
}
