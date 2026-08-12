//! Compiles a snippet against the built library and asserts the exact
//! diagnostic code it fails with.
//!
//! A `compile_fail` doctest is the cheaper form and does not make this
//! claim: on stable, rustdoc accepts the `Exxxx` annotation and ignores it,
//! so a `compile_fail,E0599` doctest passes when the snippet fails with an
//! unrelated E0308 — or with a typo. A type-level invariant whose whole
//! content is *which* refusal fires needs the code checked.
//!
//! Every caller owes a control test that must **succeed** (see
//! [`compile`]): without one, a broken rustc invocation makes every
//! failure assertion in the file pass for the wrong reason.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

/// `target/<profile>/deps`, where the compiled library and its
/// dependencies' metadata live.
fn deps_dir() -> PathBuf {
    Path::new(env!("CARGO_BIN_EXE_rwv"))
        .parent()
        .expect("the test binary lives under target/<profile>")
        .join("deps")
}

/// Why [`select_rlib`] could not settle on a path.
#[derive(Debug)]
pub(crate) enum RlibSelectionError {
    /// `read_dir` produced no name matching `librepoweave-*.rlib` at all.
    NoNamesMatched,
    /// One or more names matched, but the stat on every one of them failed.
    AllStatsFailed { names: Vec<PathBuf> },
}

/// A chosen rlib, plus any matched name whose stat failed and was excluded
/// from the choice rather than silently dropped.
#[derive(Debug)]
pub(crate) struct RlibSelection {
    pub(crate) path: PathBuf,
    pub(crate) skipped: Vec<PathBuf>,
}

/// Pick the newest-by-mtime path in `matched` for which `stat` succeeds.
///
/// `stat` is a seam: production passes real `metadata().modified()` calls,
/// which race a concurrent rebuild that unlinks and relinks the same name;
/// tests pass a closure that fails on chosen names instead, so that failure
/// path is exercised without racing an actual rebuild.
///
/// Newest-by-mtime is a heuristic, not a guarantee: `cargo build --release`
/// and `cargo test --release` unify features differently and can leave two
/// differently-built `librepoweave-*.rlib` files on disk at once, and this
/// picks whichever is newer with no way to know which one actually built
/// the running test binary.
pub(crate) fn select_rlib(
    matched: Vec<PathBuf>,
    stat: impl Fn(&Path) -> Option<SystemTime>,
) -> Result<RlibSelection, RlibSelectionError> {
    if matched.is_empty() {
        return Err(RlibSelectionError::NoNamesMatched);
    }
    let mut stated = Vec::with_capacity(matched.len());
    let mut skipped = Vec::new();
    for path in matched {
        match stat(&path) {
            Some(modified) => stated.push((modified, path)),
            None => skipped.push(path),
        }
    }
    if stated.is_empty() {
        return Err(RlibSelectionError::AllStatsFailed { names: skipped });
    }
    stated.sort_by_key(|(modified, _)| *modified);
    let (_, path) = stated.pop().expect("checked non-empty above");
    Ok(RlibSelection { path, skipped })
}

/// The freshest `librepoweave-*.rlib` in the deps directory.
fn repoweave_rlib() -> PathBuf {
    let deps = deps_dir();
    let matched: Vec<PathBuf> = std::fs::read_dir(&deps)
        .unwrap_or_else(|e| panic!("read {}: {e}", deps.display()))
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("librepoweave-") && n.ends_with(".rlib"))
        })
        .collect();

    match select_rlib(matched, |p| p.metadata().ok()?.modified().ok()) {
        Ok(RlibSelection { path, skipped }) => {
            if !skipped.is_empty() {
                // A passing test's stderr is captured and discarded by
                // cargo, so this is silent on the common path and surfaces
                // only alongside a failure that needs explaining.
                eprintln!(
                    "compile_probe: stat failed for {} of the matched \
                     librepoweave-*.rlib name(s) in {}; picked {} from the \
                     rest. A concurrent release build sharing this target \
                     directory is the likely cause: {skipped:?}",
                    skipped.len(),
                    deps.display(),
                    path.display(),
                );
            }
            path
        }
        Err(RlibSelectionError::NoNamesMatched) => panic!(
            "no librepoweave-*.rlib in {}\n\
             This is a missing build artifact, not a failed type-level \
             invariant: every probe in this file, control included, fails \
             the same way when the rlib is absent. Either the release lib \
             has never been built here, or a concurrent release build \
             sharing this target directory unlinked it between its delete \
             and rename steps — both leave zero matching names in a \
             directory listing. Re-run the suite; if it persists, \
             `cargo build --release --lib` first.",
            deps.display()
        ),
        Err(RlibSelectionError::AllStatsFailed { names }) => panic!(
            "found {} librepoweave-*.rlib name(s) in {} but a stat failed \
             for every one of them: {names:?}\n\
             A stat failing right after a directory listing enumerated the \
             name means something else removed or replaced the file in \
             between — most likely a concurrent release build sharing this \
             target directory, not a missing artifact. Re-run the suite; \
             if it persists, `cargo build --release --lib` first.",
            names.len(),
            deps.display()
        ),
    }
}

/// Compile `snippet` as a library against the built `repoweave`, returning
/// `(compiled, stderr)`.
///
/// `--emit=metadata` stops before codegen: the snippets exist to be
/// type-checked, and nothing here needs to link or run.
///
/// Only `repoweave` and `std` are in scope for a snippet — no `--extern` is
/// passed for the crate's own dependencies, so a probe that names `anyhow`
/// fails to compile for a reason that has nothing to do with what it pins.
pub fn compile(snippet: &str) -> (bool, String) {
    let tmp = crate::common::tempdir().expect("tempdir");
    let src = tmp.path().join("probe.rs");
    std::fs::write(&src, snippet).expect("write probe");

    let out = Command::new("rustc")
        .arg("--edition=2021")
        .arg("--crate-type=lib")
        .arg("--emit=metadata")
        .arg("--out-dir")
        .arg(tmp.path())
        .arg("--extern")
        .arg(format!("repoweave={}", repoweave_rlib().display()))
        .arg("-L")
        .arg(format!("dependency={}", deps_dir().display()))
        .arg(&src)
        .output()
        .expect("run rustc");

    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Assert `snippet` fails to compile with exactly `code`, and say what it
/// did instead when it does not.
pub fn assert_fails_with(code: &str, what: &str, snippet: &str) {
    assert_fails_n_times(code, 1, what, snippet);
}

/// As [`assert_fails_with`], but requiring `code` at least `n` times.
///
/// A snippet that violates the same invariant on both sides of an operator
/// emits one error per side, and a `contains` check is satisfied by either
/// one alone — so such a snippet keeps failing after half the invariant is
/// gone. Where the count is the point, it is asserted.
pub fn assert_fails_n_times(code: &str, n: usize, what: &str, snippet: &str) {
    let (compiled, stderr) = compile(snippet);
    assert!(
        !stderr.contains("error[E0514]"),
        "{what}: the probe compiler disagrees with the one that built the \
         library, so every assertion below would pass for the wrong reason:\n{stderr}"
    );
    assert!(
        !compiled,
        "{what}: expected {code}, but the snippet COMPILED — the invariant \
         is not enforced"
    );
    let seen = stderr.matches(&format!("error[{code}]")).count();
    assert!(
        seen >= n,
        "{what}: expected {code} at least {n}x, saw it {seen}x in:\n{stderr}"
    );
}
