//! Pins the single-mint invariant for the `rwv.generated` JSON marker key:
//! `RwvGeneratedMarker::KEY` in `src/integrations/merge.rs` is the sole
//! definition, and every consumer (vscode-workspace) references that
//! constant rather than re-minting the string literal. A second mint drifts
//! silently — the two only stay in sync as long as nobody edits one and
//! forgets the other.

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

/// Occurrences of the `"rwv.generated"` string literal in `text`, outside
/// comment lines and outside an in-file `#[cfg(test)]` module — a test
/// fixture legitimately quotes the marker's on-disk shape (see
/// `merge.rs`'s `vscode_marker_accepted_as_bool`), and that quote is not a
/// second mint.
fn count_literal(text: &str) -> usize {
    let mut count = 0;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("#[cfg(test)]") {
            break;
        }
        if trimmed.starts_with("//") {
            continue;
        }
        if trimmed.contains("\"rwv.generated\"") {
            count += 1;
        }
    }
    count
}

#[test]
fn rwv_generated_marker_is_minted_exactly_once() {
    let src = src_dir();
    let mut files = Vec::new();
    rust_files(&src, &mut files);

    let mut mints: Vec<String> = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read source file");
        let n = count_literal(&text);
        if n > 0 {
            let rel = file
                .strip_prefix(&src)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            for _ in 0..n {
                mints.push(rel.clone());
            }
        }
    }

    assert_eq!(
        mints,
        vec!["integrations/merge.rs".to_string()],
        "\"rwv.generated\" must be minted in exactly one place \
         (RwvGeneratedMarker::KEY in merge.rs) — every other site \
         references that constant. Found mints at: {mints:?}"
    );
}
