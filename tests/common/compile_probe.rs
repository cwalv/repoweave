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

/// Why [`select_rlib`] could not settle on a path.
#[derive(Debug)]
pub(crate) enum RlibSelectionError {
    /// `read_dir` produced no name matching `librepoweave-*.rlib` at all.
    NoNamesMatched,
    /// One or more names matched, but reading the crate identity out of every
    /// one of them failed (the byte read errored, or the crate marker was
    /// absent from the archive).
    AllIdentitiesUnreadable { names: Vec<PathBuf> },
    /// Names matched and were readable, but not one carried the crate
    /// identity the running test binary is linked against.
    NoIdentityMatch {
        /// The identity extracted from the test binary — the exact ID no
        /// candidate rlib carried.
        wanted: CrateIdentity,
        /// Every candidate name and the identity it did carry (or `None`
        /// for candidates whose identity was unreadable and reported
        /// separately in `skipped`).
        candidates: Vec<(PathBuf, Option<CrateIdentity>)>,
    },
    /// More than one candidate rlib carried the exact same crate identity as
    /// the test binary. Two artifacts sharing a StableCrateId is a broken
    /// invariant of the toolchain; refusing keeps a coin-flip out of the
    /// selection.
    AmbiguousIdentityMatch {
        wanted: CrateIdentity,
        matched: Vec<PathBuf>,
    },
}

/// A chosen rlib, plus any matched name whose crate-identity read failed and
/// was excluded from the choice rather than silently dropped.
#[derive(Debug)]
pub(crate) struct RlibSelection {
    pub(crate) path: PathBuf,
    pub(crate) skipped: Vec<PathBuf>,
}

/// The mangled crate identity rustc bakes into every symbol of a given
/// crate.
///
/// rustc mangles the crate's 64-bit `StableCrateId` into an ASCII marker of
/// the form `Cs<base62-of-id>_<len><crate_name>` and stamps it on every
/// symbol the crate defines. The compiled test binary carries the marker
/// of the `repoweave` crate it was linked against; each `librepoweave-*.rlib`
/// carries the marker of the crate archived in it. Matching one against the
/// other identifies the exact artifact rustc chose at link time — no other
/// signal on disk (mtime, filename hash, alphabetical order) does that.
///
/// Stored as `Vec<u8>` because the marker is read as raw bytes from the
/// binary and the archive, and never rendered anywhere it needs to be
/// UTF-8 (it always is, but we do not depend on it).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CrateIdentity(pub(crate) Vec<u8>);

impl std::fmt::Display for CrateIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&String::from_utf8_lossy(&self.0))
    }
}

/// Scan `bytes` for the one mangled `Cs...repoweave` marker rustc stamps on
/// every symbol from the `repoweave` crate. Returns:
///
/// - `Ok(id)` when exactly one such marker is present;
/// - `Err(None)` when no marker is present (nothing to key on);
/// - `Err(Some(all))` when multiple distinct markers are present, listing
///   every one seen so the caller can name the ambiguity.
///
/// The marker's shape is fixed by rustc's v0 mangling scheme: literal `Cs`,
/// a base62-encoded 64-bit `StableCrateId` (1 to 11 chars), a literal `_`,
/// the ASCII-decimal length of the crate name (here `9` for `repoweave`),
/// then the crate name. Scanning as bytes avoids depending on `nm`,
/// `readelf`, or an ELF-parsing crate — the marker is ASCII text embedded in
/// both binaries and archives regardless of container format.
pub(crate) fn extract_crate_identity(
    bytes: &[u8],
) -> Result<CrateIdentity, Option<Vec<CrateIdentity>>> {
    // Prefix `Cs`, then 1..=11 base62 chars, then `_9repoweave`.
    const PREFIX: &[u8] = b"Cs";
    const SUFFIX: &[u8] = b"_9repoweave";
    const MIN_ID: usize = 1;
    const MAX_ID: usize = 11;

    fn is_base62(b: u8) -> bool {
        b.is_ascii_alphanumeric()
    }

    let mut found: std::collections::BTreeSet<Vec<u8>> = std::collections::BTreeSet::new();
    let mut i = 0;
    while i + PREFIX.len() + MIN_ID + SUFFIX.len() <= bytes.len() {
        if &bytes[i..i + PREFIX.len()] != PREFIX {
            i += 1;
            continue;
        }
        // Consume as many base62 chars as possible (up to MAX_ID), then
        // check that the suffix follows.
        let id_start = i + PREFIX.len();
        let mut id_end = id_start;
        while id_end < bytes.len() && id_end - id_start < MAX_ID && is_base62(bytes[id_end]) {
            id_end += 1;
        }
        let id_len = id_end - id_start;
        if id_len >= MIN_ID
            && id_end + SUFFIX.len() <= bytes.len()
            && &bytes[id_end..id_end + SUFFIX.len()] == SUFFIX
        {
            let end = id_end + SUFFIX.len();
            found.insert(bytes[i..end].to_vec());
            i = end;
        } else {
            i += 1;
        }
    }

    match found.len() {
        0 => Err(None),
        1 => Ok(CrateIdentity(found.into_iter().next().unwrap())),
        _ => Err(Some(found.into_iter().map(CrateIdentity).collect())),
    }
}

/// Pick the `librepoweave-*.rlib` in `matched` whose baked crate identity
/// equals `wanted`.
///
/// `read_id` is a seam: production reads the file's bytes and calls
/// [`extract_crate_identity`]; tests plant fixture bytes or hand back a
/// direct `Some(id) / None` per candidate, so the failure paths are
/// exercised without racing an actual rebuild.
///
/// This is the correctness fix for the hazard rwv-lxbd measured and
/// rwv-j3qm documented: `cargo build --release` and `cargo test --release`
/// resolve features differently, so two `librepoweave-*.rlib` files with
/// different metadata hashes can coexist in `target/release/deps/`. Selecting
/// the newer one by `mtime` picks whichever build ran last — which need not
/// be the one the running test binary was linked against. Selection has to
/// be keyed on something that identifies the linked artifact; the crate
/// identity rustc bakes into both the test binary and each rlib does that.
///
/// Refusal is preferred to a fallback everywhere: a fallback to mtime here
/// would resurrect the exact hazard this replaces.
pub(crate) fn select_rlib(
    matched: Vec<PathBuf>,
    wanted: &CrateIdentity,
    read_id: impl Fn(&Path) -> Option<CrateIdentity>,
) -> Result<RlibSelection, RlibSelectionError> {
    if matched.is_empty() {
        return Err(RlibSelectionError::NoNamesMatched);
    }
    let mut candidates: Vec<(PathBuf, Option<CrateIdentity>)> = Vec::with_capacity(matched.len());
    let mut skipped = Vec::new();
    for path in matched {
        let id = read_id(&path);
        if id.is_none() {
            skipped.push(path.clone());
        }
        candidates.push((path, id));
    }
    if candidates.iter().all(|(_, id)| id.is_none()) {
        return Err(RlibSelectionError::AllIdentitiesUnreadable { names: skipped });
    }
    let matches: Vec<PathBuf> = candidates
        .iter()
        .filter(|(_, id)| id.as_ref() == Some(wanted))
        .map(|(p, _)| p.clone())
        .collect();
    match matches.len() {
        0 => Err(RlibSelectionError::NoIdentityMatch {
            wanted: wanted.clone(),
            candidates,
        }),
        1 => Ok(RlibSelection {
            path: matches.into_iter().next().unwrap(),
            skipped,
        }),
        _ => Err(RlibSelectionError::AmbiguousIdentityMatch {
            wanted: wanted.clone(),
            matched: matches,
        }),
    }
}

/// Holds a `repoweave`-defined symbol in every test binary that links this
/// module, which is what gives [`running_test_crate_identity`] a marker to
/// read.
///
/// Linking the crate does not by itself put its identity in the binary. A
/// binary whose every use of `repoweave` is generic, inlined or
/// const-evaluated defines no symbol rustc stamped with that crate's
/// identity, and which instantiations survive optimization shifts with the
/// metadata hash — so the same source is identifiable when this package
/// builds as a workspace member and unidentifiable when it builds standalone,
/// which is how CI builds it. Referencing a concrete non-generic function
/// makes the marker a property of the link rather than of the optimizer.
#[used]
static REPOWEAVE_IDENTITY_ANCHOR: fn() -> &'static str = repoweave::rwv_version;

/// The crate identity the currently-running test binary was linked against
/// for the `repoweave` crate.
///
/// This reads the running test executable's own bytes and extracts the one
/// `Cs...repoweave` marker rustc stamped on the symbols the binary carries
/// from that crate. The whole point of the exercise is that this ID
/// identifies the exact `librepoweave-*.rlib` on disk that fed the link —
/// not the newest one, not the alphabetically-first one.
fn running_test_crate_identity() -> CrateIdentity {
    let exe = std::env::current_exe().unwrap_or_else(|e| panic!("current_exe failed: {e}"));
    let bytes = std::fs::read(&exe).unwrap_or_else(|e| {
        panic!(
            "read of running test binary {} failed: {e}\n\
             The compile_probe support code needs to read its own binary to \
             identify the librepoweave-*.rlib it was linked against; without \
             it the probe cannot key selection on anything sound.",
            exe.display()
        )
    });
    match extract_crate_identity(&bytes) {
        Ok(id) => id,
        Err(None) => panic!(
            "no `Cs...repoweave` marker found in running test binary {}\n\
             rustc stamps a `Cs<StableCrateId>_9repoweave` marker on the \
             symbols a binary carries from the repoweave crate, and \
             REPOWEAVE_IDENTITY_ANCHOR exists so that this binary carries \
             one. Its absence means the anchor was dropped or the mangling \
             scheme changed under us. The compile_probe cannot key rlib \
             selection without it and refuses to guess.",
            exe.display()
        ),
        Err(Some(all)) => {
            let names: Vec<String> = all.iter().map(|id| id.to_string()).collect();
            panic!(
                "found {} distinct `Cs...repoweave` markers in running test \
                 binary {}: {:?}\n\
                 A single link produces a single StableCrateId per crate; \
                 multiple markers mean two repoweave crates were linked into \
                 one binary, which the compile_probe cannot pick between and \
                 refuses to guess about.",
                all.len(),
                exe.display(),
                names,
            )
        }
    }
}

/// Read `path` and return the one `Cs...repoweave` marker inside, or `None`
/// when the read fails or the marker is absent / ambiguous.
///
/// A `None` here is the read-side seam counterpart to
/// [`RlibSelectionError::AllIdentitiesUnreadable`] and to the "skipped"
/// bookkeeping in [`RlibSelection`]: production callers get `None` for any
/// candidate whose identity cannot be pinned down, so [`select_rlib`] can
/// treat it as excluded-with-reason rather than silently dropped.
fn read_rlib_identity(path: &Path) -> Option<CrateIdentity> {
    let bytes = std::fs::read(path).ok()?;
    extract_crate_identity(&bytes).ok()
}

/// The `librepoweave-*.rlib` in the deps directory that carries the same
/// crate identity as the running test binary — i.e. the exact artifact
/// rustc chose at link time.
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

    let wanted = running_test_crate_identity();

    match select_rlib(matched, &wanted, read_rlib_identity) {
        Ok(RlibSelection { path, skipped }) => {
            if !skipped.is_empty() {
                // A passing test's stderr is captured and discarded by
                // cargo, so this is silent on the common path and surfaces
                // only alongside a failure that needs explaining.
                eprintln!(
                    "compile_probe: crate-identity read failed for {} of the \
                     matched librepoweave-*.rlib name(s) in {}; picked {} \
                     from the rest. A concurrent release build sharing this \
                     target directory is the likely cause: {skipped:?}",
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
        Err(RlibSelectionError::AllIdentitiesUnreadable { names }) => panic!(
            "found {} librepoweave-*.rlib name(s) in {} but reading a crate \
             identity from every one of them failed: {names:?}\n\
             A read failing right after a directory listing enumerated the \
             name means something else removed or replaced the file in \
             between — most likely a concurrent release build sharing this \
             target directory, not a missing artifact. Re-run the suite; \
             if it persists, `cargo build --release --lib` first.",
            names.len(),
            deps.display()
        ),
        Err(RlibSelectionError::NoIdentityMatch { wanted, candidates }) => {
            let listing: Vec<String> = candidates
                .iter()
                .map(|(p, id)| match id {
                    Some(id) => format!("{} -> {id}", p.display()),
                    None => format!("{} -> <unreadable>", p.display()),
                })
                .collect();
            panic!(
                "no librepoweave-*.rlib in {} carries the crate identity the \
                 running test binary was linked against ({wanted}).\n\
                 Candidates seen: [\n  {}\n]\n\
                 This is the specific hazard rwv-lxbd measured and this code \
                 refuses to guess through: `cargo build --release` and \
                 `cargo test --release` resolve features differently and can \
                 leave two differently-built rlibs on disk, and the one the \
                 test binary needs is not present here. The old fallback to \
                 mtime would have picked whichever happened to be newer — \
                 possibly the build variant that never linked into this test \
                 — and the probe would then have proved something about a \
                 crate no one is exercising. Run `cargo test --release \
                 --no-run` to rebuild the test-variant artifact, then re-run \
                 the suite.",
                deps.display(),
                listing.join(",\n  "),
            )
        }
        Err(RlibSelectionError::AmbiguousIdentityMatch { wanted, matched }) => panic!(
            "found {} librepoweave-*.rlib name(s) in {} carrying the same \
             crate identity ({wanted}) as the running test binary: \
             {matched:?}\n\
             Two artifacts sharing a StableCrateId is a broken invariant of \
             the toolchain — the compile_probe cannot pick between them and \
             refuses to guess. Remove the duplicate and re-run.",
            matched.len(),
            deps.display(),
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
