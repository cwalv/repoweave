//! The git backend decides nothing from what git wrote to stderr.
//!
//! git's messages are for the operator: they are translated, and they change
//! between releases. A decision rests on an exit status, a format git
//! documents for machines, or the repository's state. Text from stderr is
//! carried into an error or shown, never matched.
//!
//! The scan reads the production lines of `src/git.rs` and `src/git/`. It
//! follows stderr text from every `stderr` it names, through `let` bindings
//! and into the parameters of the backend's own functions it is handed to,
//! and along each method chain, including out through the call it is an
//! argument of. A chain that matches a pattern against the text, compares it,
//! or asks whether git wrote anything at all is a decision on git's words.
//!
//! Residue: text that reaches a decision another way (a struct field, a
//! closure capture, a function outside the backend) is not followed.

mod common;

use common::src_scan::{production_lines, SourceLine};
use std::collections::BTreeSet;

/// Sites allowed to read git's words, each with the reason no exit status,
/// machine format or state answers the question. Matched against the
/// site's file and a needle on its line.
struct Allowed {
    file: &'static str,
    needle: &'static str,
    reason: &'static str,
}

const ALLOWED: &[Allowed] = &[];

/// Methods whose answer depends on the characters of the text they are
/// called on, as opposed to its shape.
const MATCHING: &[&str] = &[
    "contains",
    "starts_with",
    "ends_with",
    "strip_prefix",
    "strip_suffix",
    "find",
    "rfind",
    "matches",
    "rmatches",
    "match_indices",
    "rmatch_indices",
    "split_once",
    "rsplit_once",
    "trim_start_matches",
    "trim_end_matches",
    "trim_matches",
    "replace",
    "replacen",
    "eq",
    "ne",
    "cmp",
    "partial_cmp",
];

/// Asking whether git wrote anything, of the text as a whole.
const SHAPE: &[&str] = &["is_empty", "len"];

/// Methods that leave the text whole, so a [`SHAPE`] question after them is
/// still asked of everything git wrote.
const WHOLE: &[&str] = &[
    "trim",
    "trim_start",
    "trim_end",
    "as_str",
    "as_bytes",
    "as_ref",
    "to_string",
    "to_owned",
    "into_owned",
];

fn backend(file: &str) -> bool {
    file == "git.rs" || file.starts_with("git/")
}

#[derive(Debug)]
struct Site {
    file: String,
    line: usize,
    text: String,
}

/// One file's production text with comments and literal contents blanked,
/// and the line each byte came from.
struct Source {
    file: String,
    code: Vec<u8>,
    line_of: Vec<usize>,
    lines: Vec<(usize, String)>,
}

impl Source {
    fn of(file: &str, lines: &[&SourceLine]) -> Source {
        let mut code = Vec::new();
        let mut line_of = Vec::new();
        for line in lines {
            for byte in line.text.bytes().chain(*b"\n") {
                code.push(byte);
                line_of.push(line.line);
            }
        }
        blank_literals_and_comments(&mut code);
        Source {
            file: file.to_owned(),
            code,
            line_of,
            lines: lines.iter().map(|l| (l.line, l.text.clone())).collect(),
        }
    }

    fn site(&self, at: usize) -> Site {
        let line = self.line_of[at];
        let text = self
            .lines
            .iter()
            .find(|(number, _)| *number == line)
            .map(|(_, text)| text.trim().to_owned())
            .unwrap_or_default();
        Site {
            file: self.file.clone(),
            line,
            text,
        }
    }
}

fn blank_literals_and_comments(code: &mut [u8]) {
    let mut i = 0;
    while i < code.len() {
        match code[i] {
            b'/' if code.get(i + 1) == Some(&b'/') => {
                while i < code.len() && code[i] != b'\n' {
                    code[i] = b' ';
                    i += 1;
                }
            }
            b'"' => {
                let raw = i > 0 && code[i - 1] == b'r'
                    || i > 1 && code[i - 1] == b'#' && code[i - 2] == b'r';
                i += 1;
                while i < code.len() && code[i] != b'"' {
                    if code[i] == b'\\' && !raw {
                        code[i] = b' ';
                        i += 1;
                    }
                    if code[i] != b'\n' {
                        code[i] = b' ';
                    }
                    i += 1;
                }
                i += 1;
            }
            b'\'' if code.get(i + 1) == Some(&b'\\') => {
                let end = (i + 2..code.len().min(i + 12)).find(|&j| code[j] == b'\'');
                if let Some(end) = end {
                    code[i + 1..end].fill(b' ');
                    i = end + 1;
                } else {
                    i += 1;
                }
            }
            b'\'' if code.get(i + 2) == Some(&b'\'') => {
                code[i + 1] = b' ';
                i += 3;
            }
            _ => i += 1,
        }
    }
}

fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Every whole-word occurrence of `word` in `code`.
fn occurrences(code: &[u8], word: &str) -> Vec<usize> {
    let word = word.as_bytes();
    (0..code.len().saturating_sub(word.len() - 1))
        .filter(|&i| {
            &code[i..i + word.len()] == word
                && (i == 0 || !is_ident(code[i - 1]))
                && code.get(i + word.len()).is_none_or(|b| !is_ident(*b))
        })
        .collect()
}

fn skip_ws(code: &[u8], mut at: usize) -> usize {
    while at < code.len() && code[at].is_ascii_whitespace() {
        at += 1;
    }
    at
}

fn back_ws(code: &[u8], mut at: usize) -> usize {
    while at > 0 && code[at - 1].is_ascii_whitespace() {
        at -= 1;
    }
    at
}

/// The index one past the bracket closing the one at `open`.
fn close_of(code: &[u8], open: usize) -> usize {
    let (opener, closer) = (
        code[open],
        match code[open] {
            b'(' => b')',
            b'[' => b']',
            b'<' => b'>',
            _ => b'}',
        },
    );
    let mut depth = 0;
    for (i, &byte) in code.iter().enumerate().skip(open) {
        if byte == opener {
            depth += 1;
        } else if byte == closer {
            depth -= 1;
            if depth == 0 {
                return i + 1;
            }
        }
    }
    code.len()
}

/// The `(` of the call whose argument list holds `at`, if `at` is directly
/// in one.
fn enclosing_call(code: &[u8], at: usize) -> Option<usize> {
    let mut depth = 0;
    for i in (0..at).rev() {
        match code[i] {
            b')' | b']' | b'}' => depth += 1,
            b'(' if depth == 0 => {
                let before = back_ws(code, i);
                return (before > 0 && (is_ident(code[before - 1]) || code[before - 1] == b'>'))
                    .then_some(i);
            }
            b'[' | b'{' if depth == 0 => return None,
            b'(' | b'[' | b'{' => depth -= 1,
            b';' if depth == 0 => return None,
            _ => {}
        }
    }
    None
}

fn ident_at(code: &[u8], at: usize) -> (String, usize) {
    let end = (at..code.len())
        .find(|&i| !is_ident(code[i]))
        .unwrap_or(code.len());
    (String::from_utf8_lossy(&code[at..end]).into_owned(), end)
}

struct Link {
    method: String,
    args: String,
}

/// The method chain applied to the value ending at `at`, continued out of
/// each call that value is an argument of.
fn chain_from(code: &[u8], mut start: usize, mut at: usize) -> (Vec<Link>, usize) {
    let mut links = Vec::new();
    loop {
        loop {
            let next = skip_ws(code, at);
            match code.get(next) {
                Some(b'?') => at = next + 1,
                Some(b'.') if code.get(next + 1).is_some_and(|b| is_ident(*b)) => {
                    let (method, mut end) = ident_at(code, next + 1);
                    let mut after = skip_ws(code, end);
                    if code[after..].starts_with(b"::<") {
                        end = close_of(code, after + 2);
                        after = skip_ws(code, end);
                    }
                    let args = if code.get(after) == Some(&b'(') {
                        end = close_of(code, after);
                        String::from_utf8_lossy(&code[after..end]).into_owned()
                    } else {
                        String::new()
                    };
                    links.push(Link { method, args });
                    at = end;
                }
                _ => break,
            }
        }
        let next = skip_ws(code, at);
        if code.get(next) != Some(&b')') {
            return (links, at);
        }
        match enclosing_call(code, start) {
            Some(open) if close_of(code, open) == next + 1 => {
                start = open;
                at = next + 1;
            }
            _ => return (links, at),
        }
    }
}

/// Whether the occurrence of a tracked name at `at` is the text itself, as
/// opposed to a field name, a builder method or a type.
fn is_text(code: &[u8], at: usize, len: usize) -> bool {
    let after = skip_ws(code, at + len);
    let field_name = code.get(after) == Some(&b':') && code.get(after + 1) != Some(&b':');
    let before = back_ws(code, at);
    let builder = before > 0 && code[before - 1] == b'.' && code.get(after) == Some(&b'(');
    !field_name && !builder
}

/// Names bound to stderr text: `stderr` itself, every `let` whose value
/// mentions one, and every parameter of a backend function one is passed to.
fn tracked_names(sources: &[Source]) -> BTreeSet<String> {
    let mut names = BTreeSet::from(["stderr".to_owned()]);
    loop {
        let before = names.len();
        for source in sources {
            let code = &source.code;
            for name in names.clone() {
                for at in occurrences(code, &name) {
                    if !is_text(code, at, name.len()) {
                        continue;
                    }
                    if let Some(bound) = let_binding_holding(code, at) {
                        names.insert(bound);
                    }
                    if let Some(param) = parameter_receiving(sources, code, at) {
                        names.insert(param);
                    }
                }
            }
        }
        if names.len() == before {
            return names;
        }
    }
}

/// The name a `let <name> = …;` binds when `at` lies in its value.
fn let_binding_holding(code: &[u8], at: usize) -> Option<String> {
    let mut depth = 0;
    for i in (0..at).rev() {
        match code[i] {
            b')' | b']' | b'}' => depth += 1,
            b'{' if depth == 0 => return None,
            b'(' | b'[' if depth == 0 => {}
            b'(' | b'[' | b'{' => depth -= 1,
            b';' if depth == 0 => return None,
            b'=' if depth == 0
                && code.get(i + 1) != Some(&b'=')
                && i > 0
                && !matches!(code[i - 1], b'=' | b'!' | b'<' | b'>') =>
            {
                let lhs = String::from_utf8_lossy(&code[..i]);
                let statement = lhs.rsplit([';', '{', '}']).next()?.trim();
                let bound = statement.strip_prefix("let ")?.trim();
                let bound = bound.strip_prefix("mut ").unwrap_or(bound);
                let bound = bound.split(':').next()?.trim();
                return bound.bytes().all(is_ident).then(|| bound.to_owned());
            }
            _ => {}
        }
    }
    None
}

/// The parameter of a backend function that receives the argument at `at`.
fn parameter_receiving(sources: &[Source], code: &[u8], at: usize) -> Option<String> {
    let open = enclosing_call(code, at)?;
    let before = back_ws(code, open);
    let name_start = (0..before)
        .rev()
        .find(|&i| !is_ident(code[i]))
        .map_or(0, |i| i + 1);
    let callee = String::from_utf8_lossy(&code[name_start..before]).into_owned();
    let args = &code[open + 1..close_of(code, open) - 1];
    let index = top_level_commas(args)
        .into_iter()
        .filter(|&comma| comma < at - open - 1)
        .count();
    sources.iter().find_map(|source| {
        let definition = occurrences(&source.code, &format!("fn {callee}"))
            .into_iter()
            .next()?;
        let open = (definition..source.code.len()).find(|&i| source.code[i] == b'(')?;
        let params = &source.code[open + 1..close_of(&source.code, open) - 1];
        let params: Vec<String> = split_top_level(params)
            .into_iter()
            .filter(|p| !p.contains("self"))
            .collect();
        let param = params.get(index)?;
        let name = param.split(':').next()?.trim();
        let name = name.strip_prefix("mut ").unwrap_or(name);
        name.bytes().all(is_ident).then(|| name.to_owned())
    })
}

fn top_level_commas(code: &[u8]) -> Vec<usize> {
    let mut depth = 0;
    let mut commas = Vec::new();
    for (i, &byte) in code.iter().enumerate() {
        match byte {
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b')' | b']' | b'}' | b'>' => depth -= 1,
            b',' if depth == 0 => commas.push(i),
            _ => {}
        }
    }
    commas
}

fn split_top_level(code: &[u8]) -> Vec<String> {
    let mut parts = Vec::new();
    let mut from = 0;
    for comma in top_level_commas(code).into_iter().chain([code.len()]) {
        let part = String::from_utf8_lossy(&code[from..comma])
            .trim()
            .to_owned();
        if !part.is_empty() {
            parts.push(part);
        }
        from = comma + 1;
    }
    parts
}

fn reads_words(links: &[Link]) -> bool {
    links.iter().enumerate().any(|(i, link)| {
        let method = link.method.as_str();
        MATCHING.contains(&method)
            || SHAPE.contains(&method)
                && links[..i]
                    .iter()
                    .all(|l| WHOLE.contains(&l.method.as_str()))
            || MATCHING
                .iter()
                .any(|m| link.args.contains(&format!(".{m}(")))
    })
}

fn compared(code: &[u8], at: usize, end: usize) -> bool {
    let after = skip_ws(code, end);
    let before = back_ws(code, at);
    let around = [
        code.get(after..after + 2),
        before.checked_sub(2).and_then(|b| code.get(b..before)),
    ];
    around
        .iter()
        .flatten()
        .any(|pair| *pair == b"==" || *pair == b"!=")
        || String::from_utf8_lossy(&code[..before]).ends_with("match")
}

fn sources_of(lines: &[SourceLine]) -> Vec<Source> {
    let files: BTreeSet<&str> = lines
        .iter()
        .map(|l| l.file.as_str())
        .filter(|f| backend(f))
        .collect();
    files
        .into_iter()
        .map(|file| {
            let of_file: Vec<&SourceLine> = lines.iter().filter(|l| l.file == file).collect();
            Source::of(file, &of_file)
        })
        .collect()
}

fn word_readings(sources: &[Source]) -> Vec<Site> {
    let names = tracked_names(sources);
    let mut sites = Vec::new();
    for source in sources {
        let code = &source.code;
        for name in &names {
            for at in occurrences(code, name) {
                if !is_text(code, at, name.len()) {
                    continue;
                }
                let (links, end) = chain_from(code, at, at + name.len());
                if reads_words(&links) || compared(code, at, end) {
                    sites.push(source.site(at));
                }
            }
        }
    }
    sites.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    sites.dedup_by(|a, b| a.file == b.file && a.line == b.line);
    sites
}

fn allowed(site: &Site) -> bool {
    ALLOWED
        .iter()
        .any(|a| a.file == site.file && site.text.contains(a.needle) && !a.reason.is_empty())
}

#[test]
fn the_git_backend_decides_nothing_from_what_git_wrote_to_stderr() {
    let sources = sources_of(&production_lines());
    assert!(
        sources.iter().any(|s| s.file == "git.rs") && sources.len() > 1,
        "the scan reads the backend"
    );
    let reading: Vec<String> = word_readings(&sources)
        .into_iter()
        .filter(|site| !allowed(site))
        .map(|site| format!("src/{}:{}: {}", site.file, site.line, site.text))
        .collect();
    assert!(
        reading.is_empty(),
        "these sites decide from git's words, which are translated and change between \
         releases. Decide from an exit status, a format git documents for machines, or \
         the repository's state:\n{}",
        reading.join("\n")
    );
}

#[test]
fn every_allowance_still_names_a_site() {
    let sites = word_readings(&sources_of(&production_lines()));
    for allowance in ALLOWED {
        assert!(
            sites
                .iter()
                .any(|s| s.file == allowance.file && s.text.contains(allowance.needle)),
            "{}: {:?} reads nothing any more; remove the allowance",
            allowance.file,
            allowance.needle
        );
    }
}

fn readings_in(code: &str) -> Vec<usize> {
    let lines: Vec<SourceLine> = code
        .lines()
        .enumerate()
        .map(|(i, text)| SourceLine {
            file: "git.rs".to_owned(),
            line: i + 1,
            text: text.to_owned(),
        })
        .collect();
    word_readings(&sources_of(&lines))
        .into_iter()
        .map(|s| s.line)
        .collect()
}

#[test]
fn each_shape_of_reading_git_words_is_seen() {
    let code = r#"fn site() {
    if stderr.contains("commondir") {}
    Err(VcsError::CommandFailed { stderr, .. }) if stderr.trim().is_empty() => {}
    if output.stdout.is_empty() && output.stderr.is_empty() {}
    let text = String::from_utf8_lossy(&output.stderr).into_owned();
    if text.lines().any(|l| l.starts_with("error:")) {}
    if String::from_utf8_lossy(&out.stderr).trim() == "fatal" {}
    if is_revision_not_found(&stderr) {}
}
fn is_revision_not_found(message: &str) -> bool {
    message.contains("unknown revision")
}
"#;
    assert_eq!(readings_in(code), [2, 3, 4, 6, 7, 11]);
}

#[test]
fn carrying_or_showing_git_words_is_not_a_reading() {
    let code = r#"fn site() {
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    return Err(VcsError::CommandFailed { args, repo, stderr });
    command.stderr(Stdio::piped());
    let shown = format!("{stderr}\n{} has been empty", path.display());
    let cause = stderr.trim().lines().next().map_or("fetch failed", str::trim_end);
    let joined = once(summary).chain(stderr.lines().map(str::trim)).filter(|l| !l.is_empty());
    Err(failed(&args, repo, stderr))
}
fn failed(args: &[&str], repo: &Path, stderr: String) -> VcsError {
    VcsError::CommandFailed { args, repo, stderr }
}
"#;
    assert_eq!(readings_in(code), Vec::<usize>::new());
}
