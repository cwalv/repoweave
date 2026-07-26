//! Tripwire: `RWV_WORKWEAVE_DIR` must never be read in `src/` again.
//!
//! fo-ffq8a1 deleted the last read of this deprecated env var; rwv's
//! env-input inventory is `$HOME` only (docs/env-input-allowlist.txt). This
//! pins that under plain `cargo test`, rather than relying solely on
//! `cargo run --bin generate-explain`'s env-input-allowlist check, which a
//! contributor running just the test suite would never see.
//!
//! Comment lines are excluded — occurrence outside a comment is the test,
//! per the stale-symbol rule in CLAUDE.md — so this file's own header is not
//! itself a violation.

use std::path::{Path, PathBuf};

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

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

#[test]
fn rwv_workweave_dir_is_never_mentioned_outside_a_comment() {
    let src = src_dir();
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(!files.is_empty(), "no source files found under {src:?}");

    let mut offenders = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("readable source file");
        let rel = file
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .to_string();
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if line.contains("RWV_WORKWEAVE_DIR") {
                offenders.push(format!("{rel}:{} — {}", n + 1, line.trim()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "RWV_WORKWEAVE_DIR must not be consulted in src/ — rwv's env-input \
         inventory is $HOME only. Reintroducing it needs a design decision, \
         not a silent fallback:\n{}",
        offenders.join("\n")
    );
}
