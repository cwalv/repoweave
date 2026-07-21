//! Parse-validity check for `rwv <verb> [flags]` strings in user-facing output.
//!
//! Every backtick-quoted `rwv …` suggestion extracted from `src/**/*.rs` is
//! fed through `Cli::try_parse_from` so that renamed or removed verbs fail
//! the build at commit time rather than reaching users.  Semantic dead-ends
//! (e.g. a valid verb used in the wrong context) are explicitly out of scope.
//!
//! # Extraction
//!
//! The extractor walks every `*.rs` file under `src/`, strips comment-only
//! lines (`//`-prefixed after whitespace), and matches the pattern
//! `` `rwv <word>[…]` `` inside string literals.  Only the content between
//! the backticks is extracted.
//!
//! # Normalization (applied before parsing; each rule documented below)
//!
//! 1. Angle-bracket placeholders (`<source>`, `<path>`, …) → `dummy`
//! 2. Curly-brace format placeholders (`{}`, `{verb}`, `{project_name}`, …) → `dummy`
//! 3. `SCREAMING_CAPS` positional markers that are clearly placeholders (`PROJECT`,
//!    `NAME`, `SOURCE`, `PATH`, `URL`, `ROLE`) → `dummy`
//! 4. `-j N` (where `N` is literally the single token `N`) → `-j 1`  (numeric arg)
//! 5. Trailing prose punctuation (`.`, `,`, `)`, `"`) → trimmed
//!
//! # Skip rules (candidate is silently dropped; documented below)
//!
//! A. Strings containing `[…]` optional-syntax markers (e.g. `[--role ROLE]`,
//!    `[--diff]`) — they describe option sets, not concrete invocations.
//! B. Strings where the verb token itself is a placeholder after normalization
//!    (e.g. `` `rwv {verb} --continue` ``, `` `rwv {invoked}` ``,
//!    `` `rwv <verb> --continue` ``) — the verb is dynamic, not statically
//!    known, so no single parse target exists.
//! C. Strings where the second token (the verb) contains `/` — these are
//!    path-like mentions, not CLI invocations (e.g. `rwv sync /abs/source`
//!    appearing in assertion strings that explicitly say must NOT contain that
//!    form).
//!
//! # Parse verdict
//!
//! Clap error kinds distinguish two failure modes:
//!
//! - `InvalidSubcommand` (and related) — the verb or subcommand does not
//!   exist.  This is the renamed/removed-verb class this test guards.
//!
//! - `MissingRequiredArgument` — the verb path is valid but a required
//!   positional argument is absent.  Advice strings commonly name the verb
//!   and flags without the caller's own arguments (e.g. `` `rwv sync` ``
//!   tells the user to run sync with their source; `` `rwv activate` ``
//!   tells them to add a project name).  These are treated as valid.
//!
//! - `InvalidValue` — the verb and flag exist, but a normalized placeholder
//!   (`dummy`) was substituted for an enum-constrained value (e.g.
//!   `` `rwv add <url> --role <role>` `` → `rwv add dummy --role dummy` where
//!   `dummy` is not a valid `Role` variant).  The flag itself is real.
//!   These are also treated as valid (partial invocations).
//!
//! # Escape hatch
//!
//! If `src/` gains a string that looks like an invocation but is genuinely
//! not one (a false positive), add a line comment on the same or preceding
//! line containing the marker:
//!
//!   `// rwv-advice: not-an-invocation`
//!
//! The extractor skips any line whose preceding line (or the line itself)
//! carries this marker.
//!
//! # Seeded-invalid proof
//!
//! `reject_seeded_nonexistent_verb` (below) feeds a synthetic string with a
//! made-up verb through the same normalization + parse path and asserts it
//! fails.  The invalid string lives only in the test, never in `src/`.

use std::path::{Path, PathBuf};

use clap::Parser;
use repoweave::cli::{Cli, Commands};

// ---------------------------------------------------------------------------
// Source-file walker (mirrors the pattern in destructive_ops_audit_test.rs)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Normalization
// ---------------------------------------------------------------------------

/// Apply all normalization rules.  Returns `Some(normalized)` if the
/// candidate should be checked, `None` if it should be skipped.
fn normalize(raw: &str) -> Option<String> {
    // Skip rule A: optional-syntax markers like `[--role ROLE]`, `[--diff]`
    if raw.contains('[') {
        return None;
    }

    // Rule 1: angle-bracket placeholders → `dummy`
    let s = replace_angle_brackets(raw);

    // Rule 2: curly-brace format placeholders → `dummy`
    let s = replace_curly_placeholders(&s);

    // Skip rule B: verb token is itself a placeholder after substitution
    // (e.g. `rwv {verb} --continue`, `rwv dummy --continue` where dummy was
    // the verb position).
    let tokens: Vec<&str> = s.split_whitespace().collect();
    // tokens[0] is "rwv"; tokens[1] is the verb
    if tokens.len() < 2 || tokens[1] == "dummy" {
        return None;
    }

    // Skip rule C: verb token contains '/' — path-like, not a CLI verb
    if tokens[1].contains('/') {
        return None;
    }

    // Rule 3: SCREAMING_CAPS positional placeholders → `dummy`
    let s = replace_caps_placeholders(&s);

    // Rule 4: literal token `N` in `-j N` context → `1`
    let s = s
        .split_whitespace()
        .map(|t| if t == "N" { "1" } else { t })
        .collect::<Vec<_>>()
        .join(" ");

    // Rule 5: trailing prose punctuation
    let s = s.trim_end_matches(['.', ',', ')', '"']).to_owned();

    Some(s)
}

fn replace_angle_brackets(s: &str) -> String {
    let mut result = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if let Some(rel_end) = s[i..].find('>') {
                result.push_str("dummy");
                i += rel_end + 1;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

fn replace_curly_placeholders(s: &str) -> String {
    let mut result = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(rel_end) = s[i..].find('}') {
                result.push_str("dummy");
                i += rel_end + 1;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

fn replace_caps_placeholders(s: &str) -> String {
    const CAPS_TOKENS: &[&str] = &["PROJECT", "NAME", "SOURCE", "PATH", "URL", "ROLE"];
    s.split_whitespace()
        .map(|t| if CAPS_TOKENS.contains(&t) { "dummy" } else { t })
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// Extractor
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Candidate {
    /// Normalized form ready for try_parse
    normalized: String,
    /// File the candidate came from (relative to src/)
    file: String,
    /// 1-based line number in the file
    line: usize,
}

/// Scan every `*.rs` file under `src/` for backtick-quoted `rwv …` strings
/// that are not on comment-only lines and are not suppressed by the escape
/// hatch marker.
fn extract_candidates() -> Vec<Candidate> {
    let src = src_dir();
    let mut files: Vec<PathBuf> = Vec::new();
    rust_files(&src, &mut files);

    let escape_hatch = "rwv-advice: not-an-invocation";
    let mut candidates = Vec::new();

    for file in &files {
        let rel = file
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");

        let text = std::fs::read_to_string(file).expect("read source file");
        let lines: Vec<&str> = text.lines().collect();

        for (idx, &line) in lines.iter().enumerate() {
            let line_no = idx + 1; // 1-based

            // Escape hatch: preceding line or current line carries the marker
            let prev_has_marker = idx > 0 && lines[idx - 1].contains(escape_hatch);
            if prev_has_marker || line.contains(escape_hatch) {
                continue;
            }

            // Skip comment-only lines
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }

            // Extract all `rwv …` spans from this line
            let mut rest = line;
            while let Some(start) = rest.find("`rwv ") {
                rest = &rest[start + 1..]; // past the opening backtick
                let Some(end) = rest.find('`') else {
                    break; // no closing backtick — stop scanning this line
                };
                let raw = &rest[..end]; // e.g. "rwv doctor --fix"
                rest = &rest[end + 1..];

                // Guard: first word must be exactly "rwv"
                let first = raw.split_whitespace().next().unwrap_or("");
                if first != "rwv" {
                    continue;
                }

                // Guard: second word (verb) must not contain `.` or `/`
                let verb = raw.split_whitespace().nth(1).unwrap_or("");
                if verb.contains('.') || verb.contains('/') {
                    continue;
                }

                if let Some(normalized) = normalize(raw) {
                    candidates.push(Candidate {
                        normalized,
                        file: rel.clone(),
                        line: line_no,
                    });
                }
            }
        }
    }
    candidates
}

// ---------------------------------------------------------------------------
// Parse-validity verdict
// ---------------------------------------------------------------------------

/// Outcome of checking a normalized candidate against the CLI.
enum ParseVerdict {
    /// Parsed successfully — verb path and all flags are valid.
    Valid,
    /// Verb path is valid; only a required positional argument is absent.
    /// Advice strings often name the verb + flags without the caller's own
    /// arguments (e.g. `` `rwv sync` `` → the user provides the source).
    /// `MissingRequiredArgument` means the verb exists; this is not the
    /// renamed/removed-verb class this test guards.
    PartialInvocation,
    /// The subcommand or a flag does not exist.  This is the failure class
    /// this test is designed to catch.
    BadVerb,
}

fn check_parse(cmd: &str) -> ParseVerdict {
    use clap::error::ErrorKind;
    let args: Vec<&str> = cmd.split_whitespace().collect();
    match Cli::try_parse_from(&args) {
        // External-subcommand fallthrough (fo-681vre.1) makes clap accept any
        // token as a verb — it lands on `Commands::External`. But an advice
        // string that mentions e.g. `` `rwv frobnicate` `` is still meant as a
        // core-verb suggestion (advice strings are inside rwv's own source and
        // point at core surfaces), so an External match is treated as
        // BadVerb — same failure class the test has always caught.
        Ok(cli) if matches!(cli.command, Some(Commands::External(_))) => ParseVerdict::BadVerb,
        Ok(_) => ParseVerdict::Valid,
        Err(e)
            if matches!(
                e.kind(),
                // Verb path is valid; only a required positional is missing.
                ErrorKind::MissingRequiredArgument
                // Verb path and flag are valid; the value is a normalized
                // placeholder (`dummy`) that does not satisfy an enum
                // constraint (e.g. `--role dummy` when the role flag expects
                // `owned` | `reference` | …).  The flag itself exists.
                    | ErrorKind::InvalidValue
                // A bare `rwv --help` / `rwv --version` reaches clap's help
                // / version display exit path. Advice strings routinely
                // point operators at `rwv --help` (that IS the canonical
                // orientation surface), so both count as valid invocations.
                    | ErrorKind::DisplayHelp
                    | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
                    | ErrorKind::DisplayVersion
            ) =>
        {
            ParseVerdict::PartialInvocation
        }
        Err(_) => ParseVerdict::BadVerb,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Every extracted advice invocation must parse against the real CLI (or be a
/// partial invocation missing only a required positional argument supplied by
/// the user at runtime).
///
/// Failures identify renamed or removed verbs — the class this test guards.
#[test]
fn advice_invocations_all_parse() {
    let candidates = extract_candidates();

    // The extractor must find a nonzero number of advice strings.
    // An extractor that silently matches nothing is a dead check.
    assert!(
        !candidates.is_empty(),
        "extractor found zero candidates — the extraction pattern may be broken"
    );

    // Canaries: these specific normalized strings must appear in the extracted
    // set.  A missing canary means extraction or normalization has a gap.
    let canaries: &[&str] = &[
        "rwv doctor --fix",
        "rwv sync-to --continue",
        "rwv lock",
        "rwv abort",
        "rwv fetch",
        "rwv sync --continue",
    ];
    for &canary in canaries {
        assert!(
            candidates.iter().any(|c| c.normalized == canary),
            "canary {:?} not found in extracted set ({} total); \
             extraction or normalization may be too aggressive",
            canary,
            candidates.len()
        );
    }

    // Check every candidate; collect bad-verb failures.
    let mut failures: Vec<String> = Vec::new();
    for c in &candidates {
        if matches!(check_parse(&c.normalized), ParseVerdict::BadVerb) {
            failures.push(format!(
                "{}:{} — {:?} does not name a valid CLI verb",
                c.file, c.line, c.normalized
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "advice invocations that reference a non-existent verb ({} of {}):\n  {}\n\n\
         To suppress a false positive: add `// rwv-advice: not-an-invocation` \
         on the preceding line in src/.",
        failures.len(),
        candidates.len(),
        failures.join("\n  ")
    );
}

/// Seeded-invalid proof: a synthetic string with a nonexistent verb must be
/// classified as `BadVerb` by the same check path used in the main test.
/// The invalid string lives only here — never planted in `src/`.
#[test]
fn reject_seeded_nonexistent_verb() {
    let seeded = "rwv frobnicate --hard";

    // Normalization must leave this unchanged (no placeholders to substitute).
    let normalized = normalize(seeded).expect("seeded string must not be skipped");
    assert_eq!(
        normalized, seeded,
        "seeded string must survive normalization unchanged"
    );

    // The parse check must reject it as a bad verb.
    assert!(
        matches!(check_parse(&normalized), ParseVerdict::BadVerb),
        "nonexistent verb {:?} was not rejected — the parse check is not working",
        normalized
    );
}

/// Surface the extraction count for visibility during CI runs.
#[test]
fn extraction_count_is_nonzero() {
    let candidates = extract_candidates();
    println!("extracted {} advice candidates from src/", candidates.len());
    assert!(
        candidates.len() >= 10,
        "expected at least 10 candidates, found {}; extractor may be broken",
        candidates.len()
    );
}
