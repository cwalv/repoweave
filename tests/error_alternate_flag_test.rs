//! An error value is printed with the alternate flag: `{e:#}`, never `{e}`.
//!
//! `{e}` on an `anyhow::Error` prints the outermost context and drops every
//! cause beneath it; `{e:#}` prints the chain. For every other error type in
//! `src/` the two render byte-identical, because no `Display` there reads the
//! flag. So the rule is written without regard to type, and a wrong site is
//! invisible to any assertion on output until the binding it prints becomes an
//! `anyhow::Error` — which compiles clean and changes nothing visible but the
//! missing cause. That is the license for reading the source.
//!
//! **What this reads.** `src_scan::production_lines`: non-comment lines of
//! `src/`, `#[cfg(test)]` items skipped. Three shapes, each over a binding
//! named `e`, `err` or `error`:
//!
//! - an inline placeholder naming the binding, `{e}` or `{e:spec}`, whose spec
//!   carries neither `#` nor `?`;
//! - a positional or named argument that is the binding (or a reference to
//!   it), fed to a placeholder whose spec carries neither — read from every
//!   `name!(…)` call with a string-literal argument, across lines;
//! - `.to_string()` called on the binding, which renders it as `{}` does.
//!
//! **Blind spots.** An error under any other name — `source`, `why`, `cause`,
//! `primary_err` — is unread: those names also hold paths, sources and
//! strings, so matching them reports correct code. So is an error reached
//! through an expression (`self.err`, `f(e)`) rather than the bare binding,
//! an error rendered through `format_args!` handed to a function, and
//! `ToString` reached through a generic. A `/* … */` block or a trailing
//! `// …` is read as code.

use std::collections::BTreeMap;

mod common;

use common::src_scan::{self, SourceLine};

const ERROR_BINDINGS: &[&str] = &["e", "err", "error"];

/// A function that prints a binding with one of those names that holds no
/// error value. Exact both ways: the count found in that function must equal
/// `count`.
struct Exception {
    file: &'static str,
    function: &'static str,
    count: usize,
    reason: &'static str,
}

const EXCEPTIONS: &[Exception] = &[
    Exception {
        file: "check.rs",
        function: "into_text_issue",
        count: 8,
        reason: "each `error` is a `String` field of a `CheckViolation`, rendered from \
                 the error where the violation was built",
    },
    Exception {
        file: "activate.rs",
        function: "reset_ledger_no_generator_rebuilt",
        count: 1,
        reason: "`error` is the `String` `reset_unreadable_ledger` returns, already \
                 rendered",
    },
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Finding {
    file: String,
    line: usize,
    function: String,
    shape: &'static str,
}

#[derive(Default)]
struct Scan {
    findings: Vec<Finding>,
    format_strings: usize,
    unreadable: Vec<String>,
}

fn is_error_binding(text: &str) -> bool {
    ERROR_BINDINGS.contains(&text)
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn prints_without_flag(spec: &str) -> bool {
    !spec.contains('#') && !spec.contains('?')
}

fn function_name(text: &str) -> Option<&str> {
    let mut rest = text.trim_start();
    for prefix in [
        "pub(crate) ",
        "pub(super) ",
        "pub ",
        "const ",
        "async ",
        "unsafe ",
    ] {
        rest = rest.strip_prefix(prefix).unwrap_or(rest);
    }
    let rest = rest.strip_prefix("fn ")?;
    rest.split(['(', '<', ' ']).next()
}

/// `(name, spec)` of every placeholder in `text`, `{{` and `}}` skipped.
fn placeholders(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(['{', '}']) {
        let after = &rest[at + 1..];
        if after.starts_with(&rest[at..=at]) {
            rest = &after[1..];
            continue;
        }
        if rest.as_bytes()[at] == b'}' {
            rest = after;
            continue;
        }
        let Some(close) = after.find('}') else { break };
        let body = &after[..close];
        if body
            .chars()
            .all(|c| is_word_char(c) || ":#?<>^.+-$ ".contains(c))
        {
            let (name, spec) = body.split_once(':').unwrap_or((body, ""));
            out.push((name.trim().to_owned(), spec.to_owned()));
        }
        rest = &after[close + 1..];
    }
    out
}

fn inline_findings(line: &str) -> usize {
    placeholders(line)
        .iter()
        .filter(|(name, spec)| is_error_binding(name) && prints_without_flag(spec))
        .count()
}

fn to_string_findings(line: &str) -> usize {
    let needle = ".to_string()";
    let mut count = 0;
    let mut from = 0;
    while let Some(rel) = line[from..].find(needle) {
        let end = from + rel;
        let head = &line[..end];
        let start = head.trim_end_matches(is_word_char).len();
        if is_error_binding(&head[start..]) {
            count += 1;
        }
        from = end + needle.len();
    }
    count
}

/// One file's production text joined back together, so a call spanning lines
/// reads as one.
struct Joined {
    text: Vec<char>,
    line_at: Vec<usize>,
}

impl Joined {
    fn new(lines: &[&SourceLine]) -> Self {
        let mut text = Vec::new();
        let mut line_at = Vec::new();
        for line in lines {
            for c in line.text.chars().chain(['\n']) {
                text.push(c);
                line_at.push(line.line);
            }
        }
        Self { text, line_at }
    }

    /// End of the string literal opening at `at`, and its raw body, or `None`
    /// when `at` opens none.
    fn string_at(&self, at: usize) -> Option<(usize, String)> {
        let t = &self.text;
        let mut i = at;
        if t.get(i) == Some(&'b') {
            i += 1;
        }
        let mut hashes = 0;
        let raw = t.get(i) == Some(&'r');
        if raw {
            i += 1;
            while t.get(i) == Some(&'#') {
                hashes += 1;
                i += 1;
            }
        }
        if t.get(i) != Some(&'"') || (at > 0 && is_word_char(t[at - 1])) {
            return None;
        }
        let open = i + 1;
        let mut j = open;
        while j < t.len() {
            if !raw && t[j] == '\\' {
                j += 2;
                continue;
            }
            if t[j] == '"' && t[j + 1..].iter().take(hashes).all(|&c| c == '#') {
                return Some((j + 1 + hashes, t[open..j].iter().collect()));
            }
            j += 1;
        }
        Some((t.len(), String::new()))
    }

    fn char_literal_end(&self, at: usize) -> Option<usize> {
        let t = &self.text;
        if t.get(at) != Some(&'\'') {
            return None;
        }
        let len = if t.get(at + 1) == Some(&'\\') { 3 } else { 2 };
        (t.get(at + len) == Some(&'\'')).then_some(at + len + 1)
    }

    /// The top-level arguments of the call whose `(` is at `open`: each one's
    /// text, and its string-literal body when the argument is one literal.
    fn arguments(&self, open: usize) -> Option<Vec<(String, Option<String>)>> {
        let t = &self.text;
        let mut args = Vec::new();
        let mut current = String::new();
        let mut literal: Option<String> = None;
        let mut depth = 0usize;
        let mut i = open + 1;
        while i < t.len() {
            if let Some((end, body)) = self.string_at(i) {
                if current.trim().is_empty() {
                    literal = Some(body);
                }
                current.extend(&t[i..end]);
                i = end;
                continue;
            }
            if let Some(end) = self.char_literal_end(i) {
                current.extend(&t[i..end]);
                i = end;
                continue;
            }
            match t[i] {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' if depth == 0 => {
                    args.push((current.trim().to_owned(), literal));
                    return Some(args);
                }
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => {
                    args.push((current.trim().to_owned(), literal.take()));
                    current.clear();
                    i += 1;
                    continue;
                }
                _ => {}
            }
            current.push(t[i]);
            i += 1;
        }
        None
    }

    /// Every `name!(` outside a literal, by the offset of its `(`.
    fn macro_calls(&self) -> Vec<usize> {
        let t = &self.text;
        let mut out = Vec::new();
        let mut i = 0;
        while i < t.len() {
            if let Some((end, _)) = self.string_at(i) {
                i = end;
                continue;
            }
            if let Some(end) = self.char_literal_end(i) {
                i = end;
                continue;
            }
            if t[i] == '!' && i > 0 && is_word_char(t[i - 1]) && t.get(i + 1) == Some(&'(') {
                out.push(i + 1);
            }
            i += 1;
        }
        out
    }
}

/// Positional and named arguments that are an error binding fed to a
/// placeholder without the flag. Inline captures are [`inline_findings`]'.
fn argument_findings(args: &[(String, Option<String>)]) -> usize {
    let Some(format_at) = args.iter().position(|(_, literal)| literal.is_some()) else {
        return 0;
    };
    let format = args[format_at].1.as_deref().unwrap_or_default();
    let mut positional = Vec::new();
    let mut named = BTreeMap::new();
    for (text, _) in &args[format_at + 1..] {
        match text.split_once('=') {
            Some((name, value))
                if !value.starts_with('=')
                    && !name.ends_with(['!', '<', '>', '='])
                    && name.trim().chars().all(is_word_char) =>
            {
                named.insert(name.trim().to_owned(), value.trim().to_owned());
            }
            _ => positional.push(text.clone()),
        }
    }
    let mut next = 0;
    let mut count = 0;
    for (name, spec) in placeholders(format) {
        let fed = if name.is_empty() {
            next += 1;
            positional.get(next - 1)
        } else if let Ok(index) = name.parse::<usize>() {
            positional.get(index)
        } else {
            named.get(&name)
        };
        if let Some(expr) = fed {
            if is_error_binding(expr.trim_start_matches('&').trim()) && prints_without_flag(&spec) {
                count += 1;
            }
        }
    }
    count
}

fn scan(lines: &[SourceLine]) -> Scan {
    let mut by_file: BTreeMap<&str, Vec<&SourceLine>> = BTreeMap::new();
    for line in lines {
        by_file.entry(&line.file).or_default().push(line);
    }
    let mut scan = Scan::default();
    for (file, lines) in by_file {
        let mut function_at = BTreeMap::new();
        let mut function = String::new();
        let push = |scan: &mut Scan, line: usize, function: &str, shape, n: usize| {
            for _ in 0..n {
                scan.findings.push(Finding {
                    file: file.to_owned(),
                    line,
                    function: function.to_owned(),
                    shape,
                });
            }
        };
        for line in &lines {
            if let Some(name) = function_name(&line.text) {
                function = name.to_owned();
            }
            function_at.insert(line.line, function.clone());
            push(
                &mut scan,
                line.line,
                &function,
                "inline",
                inline_findings(&line.text),
            );
            push(
                &mut scan,
                line.line,
                &function,
                "to_string",
                to_string_findings(&line.text),
            );
        }
        let joined = Joined::new(&lines);
        for open in joined.macro_calls() {
            let line = joined.line_at[open];
            let Some(args) = joined.arguments(open) else {
                scan.unreadable.push(format!("{file}:{line}"));
                continue;
            };
            if args.iter().any(|(_, literal)| literal.is_some()) {
                scan.format_strings += 1;
            }
            let n = argument_findings(&args);
            push(&mut scan, line, &function_at[&line], "positional", n);
        }
    }
    scan.findings.sort();
    scan
}

#[test]
fn errors_are_printed_with_the_alternate_flag() {
    let scan = scan(&src_scan::production_lines());
    let mut failures = Vec::new();
    if !scan.unreadable.is_empty() {
        failures.push(format!(
            "macro calls whose arguments the scan could not read, so it cannot vouch for \
             them:\n    {}",
            scan.unreadable.join("\n    ")
        ));
    }

    let mut by_function: BTreeMap<(&str, &str), Vec<&Finding>> = BTreeMap::new();
    for f in &scan.findings {
        by_function
            .entry((&f.file, &f.function))
            .or_default()
            .push(f);
    }
    for ((file, function), found) in &by_function {
        let excepted = EXCEPTIONS
            .iter()
            .find(|x| x.file == *file && x.function == *function);
        if excepted.is_some_and(|x| x.count == found.len()) {
            continue;
        }
        let sites: Vec<String> = found
            .iter()
            .map(|f| format!("{}:{} ({})", f.file, f.line, f.shape))
            .collect();
        failures.push(format!(
            "{file} fn {function}: {} error binding(s) printed without `#`{}:\n    {}",
            found.len(),
            excepted.map_or(String::new(), |x| format!(
                ", {} excepted ({})",
                x.count, x.reason
            )),
            sites.join("\n    ")
        ));
    }
    for x in EXCEPTIONS {
        if !by_function.contains_key(&(x.file, x.function)) {
            failures.push(format!(
                "{} fn {}: excepted {} site(s) and none remain; drop the entry",
                x.file, x.function, x.count
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "print an error value as `{{e:#}}` so an anyhow chain survives — `{{:#}}` \
         positionally, `format!(\"{{e:#}}\")` for `.to_string()`:\n\n{}",
        failures.join("\n\n")
    );
}

#[test]
fn the_scan_reads_the_format_strings_of_src() {
    let scan = scan(&src_scan::production_lines());
    assert!(
        scan.format_strings >= 1000,
        "the scan read {} format strings from src/, which holds about 1500; its call \
         reader has stopped seeing them",
        scan.format_strings
    );
}

#[test]
fn a_planted_bare_error_is_reported_exactly() {
    let production = src_scan::production_lines();
    let host = production
        .iter()
        .position(|l| l.file == "sync.rs" && function_name(&l.text) == Some("apply_strategy"))
        .expect("sync.rs still defines apply_strategy");
    let planted = |offset: usize, text: &str| SourceLine {
        file: "sync.rs".to_owned(),
        line: 1_000_000 + offset,
        text: text.to_owned(),
    };
    let seeds = [
        planted(1, r#"    let _ = format!("planted: {e}");"#),
        planted(
            2,
            r#"    let _ = format!("planted: {} at {}", err, path.display());"#,
        ),
        planted(3, "    let _ = format!("),
        planted(4, r#"        "planted {}: {why}","#),
        planted(5, "        &error,"),
        planted(6, "        why = error,"),
        planted(7, "    );"),
        planted(8, "    let _ = err.to_string();"),
        planted(
            9,
            r#"    let _ = format!("{e:#} {err:?} {{e}} {} {entry}", e);"#,
        ),
        planted(
            10,
            r#"    let _ = format!("{:#}", err) + &self.error_count.to_string();"#,
        ),
    ];
    let mut lines = production.clone();
    lines.splice(host + 1..host + 1, seeds);

    let before = scan(&production).findings;
    let mut after = scan(&lines).findings;
    after.retain(|f| !before.contains(f));
    let reported: Vec<(usize, &str, &str)> = after
        .iter()
        .map(|f| (f.line - 1_000_000, f.shape, f.function.as_str()))
        .collect();
    assert_eq!(
        reported,
        vec![
            (1, "inline", "apply_strategy"),
            (2, "positional", "apply_strategy"),
            (3, "positional", "apply_strategy"),
            (3, "positional", "apply_strategy"),
            (8, "to_string", "apply_strategy"),
            (9, "positional", "apply_strategy"),
        ]
    );
}
