//! Reads `src/` so a pin's input is the source tree itself.
//!
//! A pin whose input is a list its author typed is a copy of the thing it
//! claims to pin, and drifts with it silently. These helpers hand a test the
//! production lines of `src/` — comment lines dropped, `#[cfg(test)]`-gated
//! items skipped by brace depth so a fixture quoting a forbidden shape is not
//! read as a live use.
//!
//! Residue, for anyone extending this: the comment filter is line-leading
//! `//` only. A `/* … */` block and a trailing `// …` after code are both
//! scanned as production text, so a needle sitting in one is a false positive
//! a caller has to notice.

use std::path::{Path, PathBuf};

/// The crate's `src/` directory.
pub fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// One production line: path relative to `src/`, 1-based line number, text.
#[derive(Debug, Clone)]
pub struct SourceLine {
    pub file: String,
    pub line: usize,
    pub text: String,
}

impl SourceLine {
    /// `file:line`, the clickable form to name a site in a failure message.
    pub fn site(&self) -> String {
        format!("{}:{}", self.file, self.line)
    }
}

/// Every production line under `src/`, in a stable file order.
pub fn production_lines() -> Vec<SourceLine> {
    let src = src_dir();
    let mut files = Vec::new();
    collect_rust_files(&src, &mut files);
    files.sort();

    let mut out = Vec::new();
    for file in &files {
        let rel = file
            .strip_prefix(&src)
            .expect("walked path is under src/")
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(file).expect("read source file");
        scan(&rel, &text, &mut out);
    }
    out
}

fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn scan(rel: &str, text: &str, out: &mut Vec<SourceLine>) {
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if trimmed.starts_with("#[cfg(test)]") {
            i = end_of_gated_item(&lines, i + 1);
            continue;
        }
        if !trimmed.starts_with("//") {
            out.push(SourceLine {
                file: rel.to_string(),
                line: i + 1,
                text: lines[i].to_string(),
            });
        }
        i += 1;
    }
}

/// Index one past the `#[cfg(test)]`-gated item beginning at `start`: a brace
/// block tracked by depth, so a nested `mod`/`fn`/macro body does not end the
/// skip early, or — for a brace-less item like `mod testing;` — its
/// terminating `;`.
fn end_of_gated_item(lines: &[&str], start: usize) -> usize {
    let mut depth: i32 = 0;
    let mut seen_open = false;
    for (offset, line) in lines[start..].iter().enumerate() {
        seen_open |= line.contains('{');
        depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
        if seen_open && depth <= 0 {
            return start + offset + 1;
        }
        if !seen_open && line.trim_end().ends_with(';') {
            return start + offset + 1;
        }
    }
    lines.len()
}

/// The struct-literal needle for `T`, derived from `T`'s own type name rather
/// than spelled out: `repoweave::integration::IntegrationContext<'_>` yields
/// `IntegrationContext {`. A rename moves the needle with the type.
pub fn struct_literal_needle<T: ?Sized>() -> String {
    let name = std::any::type_name::<T>();
    let unparameterized = name.split('<').next().unwrap_or(name);
    let short = unparameterized
        .rsplit("::")
        .next()
        .unwrap_or(unparameterized);
    format!("{short} {{")
}

/// Every string literal `text` passes as the first argument to `method`.
///
/// Matches `method("…")`, so the method's own definition and any
/// `method_something(` sharing the prefix are not call sites here.
pub fn string_arguments_to(text: &str, method: &str) -> Vec<String> {
    let opener = format!("{method}(\"");
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(&opener) {
        let after = &rest[at + opener.len()..];
        match after.find('"') {
            Some(end) => {
                out.push(after[..end].to_string());
                rest = &after[end..];
            }
            None => break,
        }
    }
    out
}
