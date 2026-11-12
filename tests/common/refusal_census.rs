//! Every refusal site in `src/`, and the test that reads the token it prints.
//!
//! Shared by `tests/refusal_census_test.rs`, which holds its prohibitions,
//! and by `cargo run --example generate-explain`, which writes what it derives
//! to `docs/internals/refusal-census.tsv`.
//!
//! **Sites.** Production lines of `src/` (comment lines and `#[cfg(test)]`
//! items dropped), outside `src/bin/` — other binaries, whose errors never
//! reach rwv's funnel — and outside `src/refusal.rs`, which defines the
//! constructors every site calls. A site is: a judged state named as
//! `State::<variant>`, recorded as `<axis> <state>`; a `RefusalKind` within
//! three lines of a `refuse!` / `refusal(` / `refusing(`, recorded as its
//! token; an `untokened!(<Class>, …)`, recorded as `(untokened:<class>)`; or a
//! bare anyhow constructor (`bail!`, `anyhow!`, `ensure!`, `Error::msg(`),
//! which names no class and is recorded as `(bare)`.
//!
//! **Reads.** A site is read by the first test file, in path order, that
//! names its token where a refusal is asserted: `State::<variant>` or
//! `RefusalKind::<variant>` among the arguments of a `refused(` call, or a
//! token string as `assert_routes_to(&stderr, "<token>"`. A judged state is
//! read only through its own state, never through its axis alone.
//!
//! **Residue.** A refusal minted outside those shapes — a typed error printed
//! through its `Display`, an `eprintln!` — is no site here. A test that
//! asserts a token any other way reads as `-`, so the read column
//! understates coverage; it does not overstate it.

use repoweave::diagnosis::State;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `(file, token-or-disposition, instrument) -> sites`.
pub type Census = BTreeMap<(String, String, String), usize>;

pub const BARE: &str = "(bare)";

/// Production lines of one file: `(line_number, text)`.
pub fn production_lines(body: &str) -> Vec<(usize, &str)> {
    let lines: Vec<&str> = body.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim_start().starts_with("#[cfg(test)]") {
            let (mut depth, mut seen) = (0i32, false);
            i += 1;
            while i < lines.len() {
                seen |= lines[i].contains('{');
                depth +=
                    lines[i].matches('{').count() as i32 - lines[i].matches('}').count() as i32;
                if seen && depth <= 0 {
                    break;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if !lines[i].trim_start().starts_with("//") {
            out.push((i + 1, lines[i]));
        }
        i += 1;
    }
    out
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{} is readable: {e}", dir.display()))
        .flatten()
    {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn relative(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

pub fn kebab(variant: &str) -> String {
    let mut out = String::new();
    for (i, c) in variant.char_indices() {
        if c.is_ascii_uppercase() {
            if i != 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The identifier `text` opens with.
fn leading_ident(text: &str) -> &str {
    &text[..text.find(|c: char| !is_ident(c)).unwrap_or(text.len())]
}

/// Each variant `text` names as `<prefix><variant>`, the prefix standing
/// alone rather than ending a longer path segment.
fn variants_after<'a>(text: &'a str, prefix: &str) -> Vec<&'a str> {
    text.match_indices(prefix)
        .filter(|(at, _)| !text[..*at].chars().next_back().is_some_and(is_ident))
        .map(|(at, _)| leading_ident(&text[at + prefix.len()..]))
        .filter(|v| !v.is_empty())
        .collect()
}

/// The judged state a production line names as `State::<variant>`.
pub fn judged_state_on(text: &str) -> Option<State> {
    variants_after(text, "State::")
        .into_iter()
        .find_map(|v| State::ALL.into_iter().find(|s| format!("{s:?}") == v))
}

pub fn judged_token(state: State) -> String {
    format!("{} {}", state.axis().token(), state.name())
}

pub fn refusal_kind_on(text: &str) -> Option<String> {
    variants_after(text, "RefusalKind::")
        .into_iter()
        .next()
        .map(str::to_owned)
}

/// The class an `untokened!(<Class>, …)` opening in `text` names, read
/// across the lines `text` joins.
pub fn untokened_class_on(text: &str) -> Option<String> {
    let at = text.find("untokened!(")?;
    let class = leading_ident(text[at + "untokened!(".len()..].trim_start());
    (!class.is_empty()).then(|| kebab(class))
}

/// Whether `text` mints an error with one of anyhow's own constructors.
pub fn bare_constructor_on(text: &str) -> bool {
    ["bail!(", "anyhow!(", "ensure!(", "Error::msg("]
        .iter()
        .any(|needle| {
            text.match_indices(needle).any(|(at, _)| {
                let before = &text[..at];
                before.ends_with("anyhow::")
                    || !before
                        .chars()
                        .next_back()
                        .is_some_and(|c| is_ident(c) || c == ':')
            })
        })
}

/// The text of the call opening at `open`, through its closing parenthesis.
fn call_text(text: &str, open: usize) -> &str {
    let mut depth = 0usize;
    for (i, c) in text[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return &text[open..open + i + 1];
                }
            }
            _ => {}
        }
    }
    &text[open..]
}

/// `code` with every string literal's contents blanked, so a call quoted in
/// a fixture string is not read as a call.
fn without_string_contents(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    let mut chars = code.chars().peekable();
    let mut prev = ' ';
    while let Some(c) = chars.next() {
        out.push(c);
        if c == '"' && !(prev == '\'' && chars.peek() == Some(&'\'')) {
            while let Some(inner) = chars.next() {
                if inner == '"' {
                    out.push('"');
                    break;
                }
                if inner == '\\' {
                    chars.next();
                    out.push_str("  ");
                    continue;
                }
                out.push(if inner == '\n' { '\n' } else { ' ' });
            }
        }
        prev = c;
    }
    out
}

/// The tokens one test file asserts a refusal of.
pub fn tokens_read_in(body: &str) -> Vec<String> {
    let code: String = body
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = Vec::new();
    let calls = without_string_contents(&code);
    for (at, _) in calls.match_indices("refused(") {
        if calls[..at].chars().next_back().is_some_and(is_ident) {
            continue;
        }
        let args = call_text(&calls, at + "refused".len());
        for v in variants_after(args, "State::") {
            if let Some(state) = State::ALL.into_iter().find(|s| format!("{s:?}") == v) {
                out.push(judged_token(state));
            }
        }
        out.extend(variants_after(args, "RefusalKind::").into_iter().map(kebab));
    }
    let routes = "assert_routes_to(&stderr, \"";
    for (at, _) in code.match_indices(routes) {
        let after = &code[at + routes.len()..];
        if let Some(end) = after.find('"') {
            out.push(after[..end].to_owned());
        }
    }
    out
}

/// Token -> the first test file, in path order, that reads it.
pub fn reads(member: &Path) -> BTreeMap<String, String> {
    let mut files = Vec::new();
    rust_files(&member.join("tests"), &mut files);
    files.sort();
    let mut out = BTreeMap::new();
    for file in files {
        let Ok(body) = std::fs::read_to_string(&file) else {
            continue;
        };
        let rel = relative(member, &file);
        for token in tokens_read_in(&body) {
            out.entry(token).or_insert_with(|| rel.clone());
        }
    }
    out
}

/// One production line's site, if it is one, as the census records it.
fn site_on(lines: &[(usize, &str)], idx: usize) -> Option<String> {
    let text = lines[idx].1;
    if let Some(state) = judged_state_on(text) {
        return Some(judged_token(state));
    }
    let before: String = lines[idx.saturating_sub(2)..=idx]
        .iter()
        .map(|(_, t)| *t)
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(variant) = refusal_kind_on(text) {
        if before.contains("refuse!") || before.contains("refusal(") || before.contains("refusing(")
        {
            return Some(kebab(&variant));
        }
    }
    if text.contains("untokened!(") {
        let ahead: String = lines[idx..(idx + 3).min(lines.len())]
            .iter()
            .map(|(_, t)| *t)
            .collect::<Vec<_>>()
            .join(" ");
        let class = untokened_class_on(&ahead).unwrap_or_else(|| "?".into());
        return Some(format!("(untokened:{class})"));
    }
    bare_constructor_on(text).then(|| BARE.to_owned())
}

/// Every site, as `(file, line, token-or-disposition)`.
pub fn sites(member: &Path) -> Vec<(String, usize, String)> {
    let mut files = Vec::new();
    rust_files(&member.join("src"), &mut files);
    files.sort();
    let mut out = Vec::new();
    for file in files {
        let rel = relative(member, &file);
        if rel.starts_with("src/bin/") || rel == "src/refusal.rs" {
            continue;
        }
        let body = std::fs::read_to_string(&file).expect("a source file is readable");
        let lines = production_lines(&body);
        for idx in 0..lines.len() {
            if let Some(token) = site_on(&lines, idx) {
                out.push((rel.clone(), lines[idx].0, token));
            }
        }
    }
    out
}

pub fn derive(member: &Path) -> Census {
    let reads = reads(member);
    let mut out = Census::new();
    for (file, _, token) in sites(member) {
        let instrument = if token.starts_with('(') {
            "-".to_owned()
        } else {
            reads.get(&token).cloned().unwrap_or_else(|| "-".into())
        };
        *out.entry((file, token, instrument)).or_default() += 1;
    }
    out
}

pub const HEADER: &str = "\
# Generated by `cargo run --example generate-explain` from tests/common/refusal_census.rs,
# which states what a site and a read are. A row whose instrument is `-` is a site
# no test drives while asserting the token it prints.
# file\ttoken-or-disposition\tinstrument-that-reads-this-site\tsites
";

pub fn render(census: &Census) -> String {
    let mut out = HEADER.to_owned();
    for ((file, token, instrument), n) in census {
        out.push_str(&format!("{file}\t{token}\t{instrument}\t{n}\n"));
    }
    out
}
