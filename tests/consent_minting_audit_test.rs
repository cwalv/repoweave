//! Consent-token minting tripwire.
//!
//! `branch-model.md` §4.4 says the CLI's flag module is "the ONLY place that
//! can construct" `DetachConsent`, `ReattachConsent` and
//! `DiscardUnmergedConsent`, and explains why the home has to be the flag
//! module: within one crate a `pub fn` constructor is callable from
//! anywhere, so "defined in `vcs.rs` but minted only by the CLI" does not
//! compile into an invariant.
//!
//! The private-field idiom delivers most of that. `DetachConsent(())`
//! cannot be written outside `cli::consent` — from any module of this crate
//! or any other — and `tests/branch_model_compile_fail_test.rs` pins each
//! token's literal route with its own probe.
//!
//! **What the compile_fail probes do NOT close, and why this file exists.**
//! `rwv`'s `[[bin]]` target is a separate crate from the `[lib]`, so
//! `pub(crate)` items are invisible to `main.rs` exactly as they are to any
//! downstream crate. `main.rs` is the real minting caller, so
//! `from_flag` has to be `pub` — and a `pub fn` returning the token IS a
//! second construction route, reachable from every module of this crate.
//! Measured, not theorised: adding
//!
//! ```ignore
//! // in src/vcs.rs
//! crate::cli::consent::DetachConsent::from_flag(true).expect("forged")
//! ```
//!
//! compiles clean. So `vcs.rs` — the module §4.4 names as the one that must
//! only ever *receive* a token — can mint one for itself, and the literal
//! probes stay green while it does, because they close a different route.
//!
//! This test is the interim guard for that gap: it pins `from_flag`'s call
//! sites to the dispatch layer, so a mint appearing in `vcs.rs`, `fetch.rs`
//! or anywhere else fails here rather than passing review. It is a static
//! call-site allowlist in the shape of `tests/destructive_ops_audit_test.rs`,
//! named for the invariant it protects, and it is **interim**: the real fix
//! is to stop having a separate binary crate mint tokens at all — move CLI
//! dispatch into the lib so `main.rs` is a thin shim and `pub(crate)`
//! becomes sufficient. Until then, this is a checked guard rather than a
//! prose one, which is the whole point of the exercise it belongs to.
//!
//! `granted()` is `pub(crate)` and needs no allowlist here: it is invisible
//! outside this crate, and every in-crate call site sits inside a
//! `#[cfg(test)]` region (verified in `src/git.rs` and `src/vcs.rs`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where `from_flag` may appear, and why.
struct Allowed {
    /// Path relative to `src/`.
    file: &'static str,
    count: usize,
    justification: &'static str,
}

const ALLOWLIST: &[Allowed] = &[
    Allowed {
        file: "cli.rs",
        count: 3,
        justification: "The three definitions themselves, one per token, in \
            `cli::consent` — the declaring module. Not call sites.",
    },
    Allowed {
        file: "main.rs",
        count: 4,
        justification: "The dispatch sites: fetch and update mint a \
            DetachConsent, doctor mints a ReattachConsent, workweave delete \
            mints a DiscardUnmergedConsent. Each maps one parsed flag to one \
            token at the boundary where the operator's intent is known, \
            which is the only place that can honestly claim consent.",
    },
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Count non-comment lines mentioning `from_flag`, per file relative to `src/`.
fn observed_counts() -> BTreeMap<String, usize> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);

    let mut counts = BTreeMap::new();
    for path in files {
        let text = std::fs::read_to_string(&path).expect("read source file");
        let hits = text
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .filter(|line| line.contains("from_flag"))
            .count();
        if hits > 0 {
            let rel = path
                .strip_prefix(&src)
                .expect("path under src")
                .to_string_lossy()
                .into_owned();
            counts.insert(rel, hits);
        }
    }
    counts
}

#[test]
fn consent_tokens_are_minted_only_at_cli_dispatch() {
    let observed = observed_counts();
    let allowed: BTreeMap<&str, &Allowed> = ALLOWLIST.iter().map(|a| (a.file, a)).collect();

    for (file, count) in &observed {
        match allowed.get(file.as_str()) {
            None => panic!(
                "{file} mints a consent token: `from_flag` appears {count}x there.\n\n\
                 A consent token is proof that the OPERATOR asked for a thing. Only the \
                 CLI dispatch layer knows that, so only it may mint one; every other \
                 module must receive a token it cannot construct (branch-model.md §4.4).\n\n\
                 `from_flag` is `pub` because the `rwv` binary is a separate crate and \
                 cannot see `pub(crate)` — which means the compiler will NOT stop you \
                 here. That is exactly why this test exists.\n\n\
                 If {file} needs consent, take the token as a parameter and let the \
                 caller thread it down from dispatch. If you are genuinely adding a new \
                 dispatch site, add {file} to this file's ALLOWLIST with a justification \
                 saying which flag it reads and why it is the operator's intent."
            ),
            Some(entry) => assert_eq!(
                count, &entry.count,
                "`from_flag` count changed in {file}: allowlist says {}, found {count}.\n\n\
                 Justification on record:\n  {}\n\n\
                 If you added a dispatch site, bump the count and extend the \
                 justification to cover it. If you added a mint anywhere else in this \
                 file, don't — thread the token down instead.",
                entry.count, entry.justification
            ),
        }
    }

    for entry in ALLOWLIST {
        assert!(
            observed.contains_key(entry.file),
            "allowlist entry for {} is stale: no `from_flag` found there. \
             Remove the entry if the minting moved.",
            entry.file
        );
    }
}
