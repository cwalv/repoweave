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

/// `target/<profile>/deps`, where the compiled library and its
/// dependencies' metadata live.
fn deps_dir() -> PathBuf {
    Path::new(env!("CARGO_BIN_EXE_rwv"))
        .parent()
        .expect("the test binary lives under target/<profile>")
        .join("deps")
}

/// The freshest `librepoweave-*.rlib` in the deps directory.
///
/// Stale artifacts from earlier builds accumulate there, so pick by
/// modification time rather than by first match.
fn repoweave_rlib() -> PathBuf {
    let deps = deps_dir();
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&deps)
        .unwrap_or_else(|e| panic!("read {}: {e}", deps.display()))
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("librepoweave-") && n.ends_with(".rlib"))
        })
        .filter_map(|p| Some((p.metadata().ok()?.modified().ok()?, p)))
        .collect();
    candidates.sort_by_key(|(modified, _)| *modified);
    candidates
        .pop()
        .map(|(_, p)| p)
        .unwrap_or_else(|| panic!("no librepoweave-*.rlib in {}", deps.display()))
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
