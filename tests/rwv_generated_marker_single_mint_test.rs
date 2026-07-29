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
/// comment lines and outside any `#[cfg(test)]`-gated item — a test
/// fixture legitimately quotes the marker's on-disk shape (see
/// `merge.rs`'s `vscode_marker_accepted_as_bool`), and that quote is not a
/// second mint. The gated item's extent is found by brace-depth tracking,
/// not by treating the first `#[cfg(test)]` as a whole-file cutoff.
fn count_literal(text: &str) -> usize {
    let mut count = 0;
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("#[cfg(test)]") {
            skip_gated_item(&mut lines);
            continue;
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

/// Consume lines through the end of whatever `#[cfg(test)]` gates: a brace
/// block (tracked by depth, so a nested `mod`/`fn`/macro body doesn't end
/// the skip early), or — for a brace-less item like `mod testing;` — its
/// terminating `;`.
fn skip_gated_item(lines: &mut std::str::Lines) {
    let mut depth: i32 = 0;
    let mut seen_open = false;
    for line in lines {
        seen_open |= line.contains('{');
        depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
        if seen_open && depth <= 0 {
            return;
        }
        if !seen_open && line.trim_end().ends_with(';') {
            return;
        }
    }
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
