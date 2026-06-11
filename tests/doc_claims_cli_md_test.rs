//! Drift gate: cli.md flag tables vs. clap `--help` output.
//!
//! Doc claims pinned here:
//!
//!   - Every `--long-flag` named in a cli.md verb flag table must appear in
//!     the corresponding `rwv <verb> --help` output.
//!
//! Design choice (b): parse cli.md flag names and diff against `--help`.
//! Option (a) (generate tables from clap) would overwrite hand-written effect
//! descriptions that are not present in clap `--help` output at that prose
//! level. (b) is the proportionate gate: names are pinned, prose stays human.
//!
//! The gate is intentionally one-directional for cli.md: cli.md may *omit*
//! flags (e.g. internal/hidden flags), but must not *invent* flags that do not
//! exist in clap. This catches the fo-0tr5vl root cause: a removed flag
//! (`--force` on sync/sync-to) remained in cli.md through a green CI.
//!
//! Anchored by `docs/reference/cli.md`.

use assert_cmd::Command;

mod common;

fn rwv() -> Command {
    common::rwv()
}

/// Read `docs/reference/cli.md` relative to the crate root.
fn read_cli_md() -> String {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR must be set (run via cargo test)");
    let path = std::path::Path::new(&manifest).join("docs/reference/cli.md");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

/// Run `rwv <args>` and return the stdout of `--help`.
fn help_flags(args: &[&str]) -> Vec<String> {
    let mut full_args: Vec<&str> = args.to_vec();
    full_args.push("--help");

    let output = rwv()
        .args(&full_args)
        .output()
        .expect("rwv --help invocation failed to start");

    // --help exits 0 on clap; allow non-zero for robustness.
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let combined = format!("{stdout}{stderr}");

    // Extract every --long-flag name from help output.
    extract_long_flags_from_help(&combined)
}

/// Extract `--flag` long-option names from `--help` output text.
///
/// Scans every whitespace-separated token for a `--` prefix. Strips
/// surrounding punctuation (backticks, parens, commas) but keeps hyphens in
/// the flag stem.
fn extract_long_flags_from_help(text: &str) -> Vec<String> {
    let mut flags = Vec::new();
    for word in text.split_whitespace() {
        // Strip leading/trailing punctuation common in help output.
        let trimmed = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '-');
        if trimmed.starts_with("--") {
            // Truncate at `=`, `<`, whitespace — keep the flag stem only.
            let stem: String = trimmed
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '-')
                .collect::<String>()
                .to_lowercase();
            if stem.len() > 2 {
                flags.push(stem);
            }
        }
    }
    flags.sort();
    flags.dedup();
    flags
}

/// Extract `--flag` long-option names from a cli.md flag table section.
///
/// The table format is:
///   `| Flag | Effect |`
///   `|---|---|`
///   `| \`--flag-name [args]\` | prose description |`
///   `| \`--flag-a\` / \`--flag-b\` | ... |`
///
/// We only scan backtick spans (`` `...` ``) within table data rows (lines
/// that start with `|` but are NOT separator rows like `|---|---|`).
/// This avoids picking up prose tokens, table separators, or reference URLs.
fn extract_cli_md_flags_for_section(cli_md: &str, section_header: &str) -> Vec<String> {
    // Find the section starting after the header line.
    let start = match cli_md.find(section_header) {
        Some(pos) => pos + section_header.len(),
        None => panic!(
            "cli.md section not found: {section_header:?}\n\
             (check that the section header matches exactly)"
        ),
    };

    // Collect lines until the next `### ` heading (or end of file).
    let section_text = &cli_md[start..];
    let end = section_text.find("\n### ").unwrap_or(section_text.len());
    let section = &section_text[..end];

    let mut flags = Vec::new();

    for line in section.lines() {
        let trimmed = line.trim();
        // Only process table data rows: start with `|`, not a separator row.
        if !trimmed.starts_with('|') {
            continue;
        }
        // Skip separator rows like `|---|---|` or `|---|---|---|`.
        if trimmed.replace(['-', '|'], "").trim().is_empty() {
            continue;
        }
        // Extract content inside backtick spans.
        let mut rest = trimmed;
        while let Some(open) = rest.find('`') {
            rest = &rest[open + 1..];
            let close = match rest.find('`') {
                Some(c) => c,
                None => break,
            };
            let span = &rest[..close];
            rest = &rest[close + 1..];

            // The span may be `--flag`, `--flag <ARG>`, `--flag|other`,
            // `--flag-a` / `--flag-b`, etc. Extract every `--token` in it.
            for token in span.split_whitespace() {
                let t = token.trim_matches(|c: char| !c.is_alphanumeric() && c != '-');
                if t.starts_with("--") {
                    let stem: String = t
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '-')
                        .collect::<String>()
                        .to_lowercase();
                    if stem.len() > 2 {
                        flags.push(stem);
                    }
                }
            }
        }
    }

    flags.sort();
    flags.dedup();
    flags
}

/// Assert every `--flag` named in `cli_md_flags` appears in `help_flags`.
///
/// Failures list all missing flags together so one run surfaces all rot.
fn assert_all_cli_md_flags_in_help(
    verb_display: &str,
    cli_md_flags: &[String],
    help_flags: &[String],
) {
    let missing: Vec<&str> = cli_md_flags
        .iter()
        .filter(|f| !help_flags.contains(f))
        .map(String::as_str)
        .collect();

    assert!(
        missing.is_empty(),
        "cli.md lists flag(s) for `rwv {verb_display}` that are absent from `--help`.\n\
         Missing: {missing:?}\n\
         \n\
         This means cli.md documents a flag that no longer exists in clap (or was renamed).\n\
         Fix: remove or rename the flag row in docs/reference/cli.md to match the binary.\n\
         \n\
         `--help` flags: {help_flags:?}"
    );
}

// ===========================================================================
// rwv fetch
// ===========================================================================

#[test]
fn cli_md_fetch_flags_exist_in_help() {
    let cli_md = read_cli_md();
    // Section starts at "### `rwv fetch <source> [...]`"
    let cli_flags = extract_cli_md_flags_for_section(&cli_md, "### `rwv fetch");
    let help = help_flags(&["fetch"]);
    assert_all_cli_md_flags_in_help("fetch", &cli_flags, &help);
}

// ===========================================================================
// rwv update
// ===========================================================================

#[test]
fn cli_md_update_flags_exist_in_help() {
    let cli_md = read_cli_md();
    let cli_flags = extract_cli_md_flags_for_section(&cli_md, "### `rwv update");
    let help = help_flags(&["update"]);
    assert_all_cli_md_flags_in_help("update", &cli_flags, &help);
}

// ===========================================================================
// rwv lock
// ===========================================================================

#[test]
fn cli_md_lock_flags_exist_in_help() {
    let cli_md = read_cli_md();
    let cli_flags = extract_cli_md_flags_for_section(&cli_md, "### `rwv lock`");
    let help = help_flags(&["lock"]);
    assert_all_cli_md_flags_in_help("lock", &cli_flags, &help);
}

// ===========================================================================
// rwv sync — the primary rot target (--force was silently kept here)
// ===========================================================================

#[test]
fn cli_md_sync_flags_exist_in_help() {
    let cli_md = read_cli_md();
    let cli_flags = extract_cli_md_flags_for_section(&cli_md, "### `rwv sync <source>");
    let help = help_flags(&["sync"]);
    assert_all_cli_md_flags_in_help("sync", &cli_flags, &help);
}

// ===========================================================================
// rwv sync-to — the primary rot target (--force was silently kept here)
// ===========================================================================

#[test]
fn cli_md_sync_to_flags_exist_in_help() {
    let cli_md = read_cli_md();
    let cli_flags = extract_cli_md_flags_for_section(&cli_md, "### `rwv sync-to");
    let help = help_flags(&["sync-to"]);
    assert_all_cli_md_flags_in_help("sync-to", &cli_flags, &help);
}

// ===========================================================================
// rwv push
// ===========================================================================

#[test]
fn cli_md_push_flags_exist_in_help() {
    let cli_md = read_cli_md();
    let cli_flags = extract_cli_md_flags_for_section(&cli_md, "### `rwv push");
    let help = help_flags(&["push"]);
    assert_all_cli_md_flags_in_help("push", &cli_flags, &help);
}

// ===========================================================================
// rwv doctor
// ===========================================================================

#[test]
fn cli_md_doctor_flags_exist_in_help() {
    let cli_md = read_cli_md();
    let cli_flags = extract_cli_md_flags_for_section(&cli_md, "### `rwv doctor");
    let help = help_flags(&["doctor"]);
    assert_all_cli_md_flags_in_help("doctor", &cli_flags, &help);
}

// ===========================================================================
// rwv workweave create
// ===========================================================================

#[test]
fn cli_md_workweave_create_flags_exist_in_help() {
    let cli_md = read_cli_md();
    let cli_flags =
        extract_cli_md_flags_for_section(&cli_md, "### `rwv workweave <project> create");
    let help = help_flags(&["workweave", "create"]);
    assert_all_cli_md_flags_in_help("workweave create", &cli_flags, &help);
}
