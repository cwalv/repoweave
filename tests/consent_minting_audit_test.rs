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
//! **`granted()` is the other minting route, and it is pinned below too.**
//! It is `pub(crate)`, so it is invisible outside this crate — but "outside
//! this crate" is not the boundary that matters when the module §4.4 says
//! must only *receive* a token lives inside it. It used to be true that
//! every non-test call site was absent; `sync.rs` now mints a
//! `DiscardLocalCommitsConsent` for the rewinding MOVE, deliberately (that
//! token's flag has a second spelling — the override recorded in the owner
//! record, which `--continue` reads back with no flags on the command
//! line). The second test here pins that surface so the next one has to be
//! argued for rather than noticed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where a minting call may appear, and why.
struct Allowed {
    /// Path relative to `src/`.
    file: &'static str,
    count: usize,
    justification: &'static str,
}

const ALLOWLIST: &[Allowed] = &[
    Allowed {
        file: "cli.rs",
        count: 4,
        justification: "The four definitions themselves, one per token, in \
            `cli::consent` — the declaring module. Not call sites.",
    },
    Allowed {
        file: "main.rs",
        count: 5,
        justification: "The dispatch sites: fetch and update mint a \
            DetachConsent, doctor mints a ReattachConsent and an \
            AdoptDetachedConsent, workweave delete mints a \
            DiscardUnmergedConsent. Each maps one parsed flag to one \
            token at the boundary where the operator's intent is known, \
            which is the only place that can honestly claim consent.",
    },
];

/// Where `granted()` may appear outside a test module, and why.
const GRANTED_ALLOWLIST: &[Allowed] = &[
    Allowed {
        file: "cli.rs",
        count: 4,
        justification: "The four definitions themselves, one per token. \
            Not call sites.",
    },
    Allowed {
        file: "vcs.rs",
        count: 1,
        justification: "The definition of DiscardLocalCommitsConsent::granted. \
            Not a call site.",
    },
    Allowed {
        file: "sync.rs",
        count: 1,
        justification: "rewind_project_repo: mints the \
            DiscardLocalCommitsConsent for the --discard-local-commits \
            rewinding MOVE. NOT dispatch, and that is the point — the flag \
            is persisted into the owner record's overrides and read back by \
            `rwv sync --continue`, which parses no flags at all. A mint at \
            dispatch would give the fresh path a token and leave the resumed \
            path — the one an operator actually re-runs after a conflict — \
            with nothing to prove the same consent with. sync.rs is the \
            layer that holds both spellings. If you are adding a mint \
            anywhere else, thread the token down instead.",
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

/// Whether the next item in `rest` (skipping blank, comment and attribute
/// lines) is a module declaration — i.e. whether the `#[cfg(test)]` just
/// seen opens a test module rather than gating a single test-only item.
///
/// Same shape as `tests/destructive_ops_audit_test.rs`, for the same
/// reason: a `#[cfg(test)] mod` in `src/` is test code that happens to live
/// next to what it tests, and its fixtures legitimately mint tokens to
/// build the states the product code is asserted against.
fn next_item_is_a_module(rest: &[&str]) -> bool {
    for line in rest {
        let t = line.trim_start();
        if t.is_empty() || t.starts_with("//") || t.starts_with("#[") {
            continue;
        }
        return t.starts_with("mod ")
            || t.starts_with("pub mod ")
            || t.starts_with("pub(crate) mod ");
    }
    false
}

/// Count non-comment lines containing `needle`, per file relative to
/// `src/`, stopping at the file's `#[cfg(test)] mod`.
fn observed_counts(needle: &str, skip_test_modules: bool) -> BTreeMap<String, usize> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);

    let mut counts = BTreeMap::new();
    for path in files {
        let text = std::fs::read_to_string(&path).expect("read source file");
        let lines: Vec<&str> = text.lines().collect();
        let mut hits = 0;
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if skip_test_modules
                && trimmed.starts_with("#[cfg(test)]")
                && next_item_is_a_module(&lines[i + 1..])
            {
                break;
            }
            if trimmed.starts_with("//") {
                continue;
            }
            if trimmed.contains(needle) {
                hits += 1;
            }
        }
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
    // `from_flag` is `pub`, so a test module is as capable of calling it as
    // product code is; this scan does not exclude them.
    let observed = observed_counts("from_flag", false);
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

#[test]
fn the_unconditional_mint_is_confined_to_its_argued_sites() {
    let observed = observed_counts("granted()", true);
    let allowed: BTreeMap<&str, &Allowed> = GRANTED_ALLOWLIST.iter().map(|a| (a.file, a)).collect();

    for (file, count) in &observed {
        match allowed.get(file.as_str()) {
            None => panic!(
                "{file} mints a consent token unconditionally: `granted()` appears \
                 {count}x there outside any test module.\n\n\
                 `granted()` takes no argument and checks nothing — holding the token \
                 is the whole proof, so a call to it is a claim that the operator asked \
                 for the thing. Only a layer that actually knows they did may make that \
                 claim (branch-model.md §4.4).\n\n\
                 It is `pub(crate)`, so the compiler will not stop you: every module of \
                 this crate can reach it. That is why this test exists.\n\n\
                 If {file} needs consent, take the token as a parameter and let the \
                 caller thread it down. If {file} genuinely is a layer that knows the \
                 operator's intent, add it to GRANTED_ALLOWLIST with a justification \
                 saying where that knowledge comes from — a parsed flag, or a durable \
                 record of one."
            ),
            Some(entry) => assert_eq!(
                count, &entry.count,
                "`granted()` count changed in {file}: allowlist says {}, found {count}.\n\n\
                 Justification on record:\n  {}\n\n\
                 Bump the count only with a justification that covers the new site.",
                entry.count, entry.justification
            ),
        }
    }

    for entry in GRANTED_ALLOWLIST {
        assert!(
            observed.contains_key(entry.file),
            "GRANTED_ALLOWLIST entry for {} is stale: no non-test `granted()` found \
             there. Remove the entry if the minting moved.",
            entry.file
        );
    }
}
