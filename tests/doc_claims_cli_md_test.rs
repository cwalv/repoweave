//! Drift gate: cli.md flag tables vs. clap `--help` output.
//!
//! Doc claims pinned here:
//!
//!   - Every `--long-flag` named in a cli.md verb flag table must appear in
//!     the corresponding `rwv <verb> --help` output.
//!   - Every *value* a doc offers for an enum-valued flag — `--strategy
//!     ff|rebase` — must be a value clap accepts, in cli.md **and** README.
//!
//! Design choice (b): parse cli.md flag names and diff against `--help`.
//! Option (a) (generate tables from clap) would overwrite hand-written effect
//! descriptions that are not present in clap `--help` output at that prose
//! level. (b) is the proportionate gate: names are pinned, prose stays human.
//!
//! The gate is intentionally one-directional for cli.md: cli.md may *omit*
//! flags (e.g. internal/hidden flags), but must not *invent* flags that do not
//! exist in clap. This catches the root cause it was built for: a removed flag
//! (`--force` on sync/sync-to) remained in cli.md through a green CI.
//!
//! # Omission is allowed; a wrong value is not
//!
//! One-directional on flag *names* is a deliberate choice — a doc may leave a
//! flag out. It was never a licence to say something false about a flag that
//! is documented, and for a long time the name check was the only check, so a
//! doc could offer a value clap rejects and CI stayed green. README advertised
//! `--strategy ff|rebase|merge` after `merge` was removed from `SyncStrategy`;
//! the name `--strategy` still existed, so nothing fired, and the first surface
//! a new user reads told them to run a command that hard-errors at parse time.
//!
//! Values are checked in both documents, because that is where the miss was:
//! cli.md was correct and README was not. A gate that reads only the reference
//! page cannot see the page most readers start on.
//!
//! The value check covers **enum-valued flags only** — the ones whose `--help`
//! carries a `Possible values:` block, so the accepted set is knowable. A
//! free-form value (`--role <ROLE>`) has no set to check against and is not
//! pinned here.
//!
//! Anchored by `docs/reference/cli.md` and `README.md`.

use assert_cmd::Command;
use std::collections::BTreeMap;

mod common;

fn rwv() -> Command {
    common::rwv()
}

/// Read a doc file relative to the crate root.
fn read_doc(rel: &str) -> String {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR must be set (run via cargo test)");
    let path = std::path::Path::new(&manifest).join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

/// Read `docs/reference/cli.md` relative to the crate root.
fn read_cli_md() -> String {
    read_doc("docs/reference/cli.md")
}

/// Run `rwv <args> --help` and return its output.
///
/// `--help` (not `-h`) is deliberate: only the long form prints the
/// `Possible values:` block the value check reads.
fn help_text(args: &[&str]) -> String {
    let mut full_args: Vec<&str> = args.to_vec();
    full_args.push("--help");

    let output = rwv()
        .args(&full_args)
        .output()
        .expect("rwv --help invocation failed to start");

    // --help exits 0 on clap; allow non-zero for robustness.
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    format!("{stdout}{stderr}")
}

/// Run `rwv <args>` and return the stdout of `--help`.
fn help_flags(args: &[&str]) -> Vec<String> {
    // Extract every --long-flag name from help output.
    common::extract_long_flags_from_help(&help_text(args))
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
// rwv materialize
// ===========================================================================

/// Added when `--remove-undeclared-links` did: this verb had no arm here, so
/// a flag documented in `cli.md` under a spelling clap does not accept would
/// have shipped green.
///
/// Note what this direction does and does not buy, because it is the one the
/// survey in `CLAUDE.md` names: it catches a documented flag the binary
/// rejects. It does NOT catch a flag the binary accepts and nobody documented
/// — that gap is real here as everywhere else in this file.
#[test]
fn cli_md_materialize_flags_exist_in_help() {
    let cli_md = read_cli_md();
    let cli_flags = extract_cli_md_flags_for_section(&cli_md, "### `rwv materialize");
    assert!(
        !cli_flags.is_empty(),
        "the materialize section yielded no flags, so this test would pass \
         against a section slicer that stopped matching"
    );
    let help = help_flags(&["materialize"]);
    assert_all_cli_md_flags_in_help("materialize", &cli_flags, &help);
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

// ===========================================================================
// Flag values: every value a doc offers must be one clap accepts
//
// The name check above answers "does this flag exist?". It cannot answer "is
// this a value the flag takes?", and that is the direction a documented
// `--strategy merge` slipped through after `merge` was removed.
// ===========================================================================

/// Accepted values for each enum-valued flag in `help`, keyed by flag name.
///
/// A flag appears here only if clap printed a `Possible values:` block for it,
/// which is exactly the set of flags whose accepted values are knowable from
/// the outside. Everything else takes a free-form string and has no set to
/// check a doc against.
fn possible_values_by_flag(help: &str) -> BTreeMap<String, Vec<String>> {
    let mut by_flag = BTreeMap::new();
    let mut current: Option<String> = None;
    let mut collecting = false;

    for line in help.lines() {
        let trimmed = line.trim();

        // `- ff:  Fast-forward only` — a value row of the block being collected.
        if collecting {
            match trimmed.strip_prefix("- ") {
                Some(rest) => {
                    let value = rest.split(':').next().unwrap_or(rest).trim();
                    if let Some(flag) = &current {
                        by_flag
                            .entry(flag.clone())
                            .or_insert_with(Vec::new)
                            .push(value.to_owned());
                    }
                    continue;
                }
                // A blank line inside the block is layout; anything else ends it.
                None if trimmed.is_empty() => continue,
                None => collecting = false,
            }
        }

        if trimmed == "Possible values:" {
            collecting = true;
            continue;
        }

        // `      --strategy <STRATEGY>` — the flag any following block belongs to.
        if let Some(word) = trimmed.split_whitespace().next() {
            if let Some(name) = word.trim_end_matches(',').strip_prefix("--") {
                if !name.is_empty() {
                    current = Some(format!("--{name}"));
                }
            }
        }
    }

    by_flag
}

/// Every `(flag, values)` a markdown text offers, from spans like
/// `` `--strategy ff\|rebase` ``.
///
/// Only backtick spans are read, and only the token immediately after the flag
/// is taken as its value spec, so prose that happens to mention a flag and a
/// word later in the sentence claims nothing. A spec holding a placeholder
/// (`<STRATEGY>`, `[args]`) is not a value claim and is dropped whole — the
/// doc is naming the shape of the argument, not offering a value for it.
///
/// The `\|` a markdown table needs to escape a cell separator is unescaped
/// first, so a table row and a prose mention parse identically.
fn extract_flag_value_claims(text: &str) -> Vec<(String, Vec<String>)> {
    let mut claims = Vec::new();

    for span in text.split('`').skip(1).step_by(2) {
        let unescaped = span.replace("\\|", "|");
        let tokens: Vec<&str> = unescaped.split_whitespace().collect();
        for (i, token) in tokens.iter().enumerate() {
            let flag = token.trim_end_matches(',');
            if !flag.starts_with("--") || flag.len() <= 2 {
                continue;
            }
            let Some(spec) = tokens.get(i + 1) else {
                continue;
            };
            let spec = spec.trim_end_matches(['.', ',', ';', ')']);
            if !spec.contains('|') {
                continue;
            }
            let values: Vec<String> = spec.split('|').map(str::to_owned).collect();
            let all_plain = values.iter().all(|v| {
                !v.is_empty()
                    && v.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            });
            if all_plain {
                claims.push((flag.to_owned(), values));
            }
        }
    }

    claims
}

/// Findings for every value `text` offers that `accepted` does not contain.
///
/// A claim naming a flag with no entry in `accepted` is skipped: either the
/// flag takes a free-form value, or it does not exist — and "does it exist" is
/// the name check's question, answered for cli.md above.
fn value_claim_errors(
    surface: &str,
    verb: &str,
    text: &str,
    accepted: &BTreeMap<String, Vec<String>>,
) -> Vec<String> {
    let mut errors = Vec::new();
    for (flag, values) in extract_flag_value_claims(text) {
        let Some(ok) = accepted.get(&flag) else {
            continue;
        };
        for value in values {
            if !ok.contains(&value) {
                errors.push(format!(
                    "{surface} offers `{flag} {value}` for `rwv {verb}`, which clap rejects \
                     at parse time. Accepted: {ok:?}"
                ));
            }
        }
    }
    errors
}

/// Assert a surface offers no value clap would reject.
fn assert_no_rejected_values(errors: &[String]) {
    assert!(
        errors.is_empty(),
        "a documented flag value is not accepted by the binary:\n{}\n\n\
         The flag exists, so the flag-name check above stays green — this is the \
         direction it cannot see.\n\
         Fix: correct the value list in the doc, or add the value to the enum.",
        errors.join("\n")
    );
}

/// Verbs whose `--help` is read for the value check. Every enum-valued flag
/// rwv has today sits on one of these; a verb with no `Possible values:` block
/// contributes an empty map and costs one process spawn.
const VALUE_CHECKED_VERBS: &[&[&str]] = &[
    &["fetch"],
    &["update"],
    &["lock"],
    &["sync"],
    &["sync-to"],
    &["push"],
    &["doctor"],
    &["add"],
    &["status"],
];

#[test]
fn cli_md_flag_values_are_accepted_by_clap() {
    let cli_md = read_cli_md();
    let mut errors = Vec::new();
    for args in VALUE_CHECKED_VERBS {
        let accepted = possible_values_by_flag(&help_text(args));
        if accepted.is_empty() {
            continue;
        }
        let verb = args.join(" ");
        let header = format!("### `rwv {verb}");
        // Only sections cli.md actually has; the name tests above pin which
        // headers exist, so a miss here is a verb cli.md does not document.
        if !cli_md.contains(&header) {
            continue;
        }
        let section = section_text(&cli_md, &header);
        errors.extend(value_claim_errors("cli.md", &verb, section, &accepted));
    }
    assert_no_rejected_values(&errors);
}

#[test]
fn readme_flag_values_are_accepted_by_clap() {
    let readme = read_doc("README.md");
    let mut errors = Vec::new();
    for args in VALUE_CHECKED_VERBS {
        let accepted = possible_values_by_flag(&help_text(args));
        if accepted.is_empty() {
            continue;
        }
        let verb = args.join(" ");
        // README's command table gives each verb one row; the row is the unit
        // of text, so a value spec is read against the verb it is offered for.
        for line in readme.lines() {
            let trimmed = line.trim();
            if !trimmed.starts_with('|') || !trimmed.contains(&format!("`rwv {verb}")) {
                continue;
            }
            errors.extend(value_claim_errors("README.md", &verb, trimmed, &accepted));
        }
    }
    assert_no_rejected_values(&errors);
}

/// The body of the `### `-delimited section whose heading starts with
/// `header`, from the line after the heading to the next `### `.
///
/// The slice starts at a line boundary, not at the end of the matched header.
/// `extract_flag_value_claims` reads backtick spans by parity, and a heading
/// like ``### `rwv sync <source>` `` holds an odd number of backticks — cutting
/// inside it inverts every span in the section, which is a silent pass, not a
/// failure. Cutting at the newline keeps the section's own parity intact.
fn section_text<'a>(md: &'a str, header: &str) -> &'a str {
    let Some(pos) = md.find(header) else {
        return "";
    };
    let body_start = md[pos..]
        .find('\n')
        .map_or(md.len(), |offset| pos + offset + 1);
    let rest = &md[body_start..];
    &rest[..rest.find("\n### ").unwrap_or(rest.len())]
}

// ---------------------------------------------------------------------------
// Pinning tests: the value check must fail on the drift it was written for
// ---------------------------------------------------------------------------

/// **The `--strategy merge` regression, as a fixture.** README carried exactly
/// this row while `SyncStrategy` held only `Ff` and `Rebase`.
///
/// If the value check is ever weakened back to name-only, this goes green and
/// says so.
#[test]
fn a_documented_value_clap_rejects_is_caught() {
    let accepted = BTreeMap::from([(
        "--strategy".to_owned(),
        vec!["ff".to_owned(), "rebase".to_owned()],
    )]);
    let row = "| `rwv sync <source>` | Pull: align CWD to another workspace's \
               committed `rwv.lock`. `--strategy ff\\|rebase\\|merge` (default `ff`) |";
    let errors = value_claim_errors("README.md", "sync", row, &accepted);
    assert_eq!(
        errors.len(),
        1,
        "the removed `merge` value must be reported once, got:\n{}",
        errors.join("\n")
    );
    assert!(
        errors[0].contains("--strategy merge"),
        "the finding must name the rejected value, got:\n{}",
        errors[0]
    );
}

/// The accepted values must pass — a check that rejects everything pins
/// nothing.
#[test]
fn documented_values_clap_accepts_are_not_reported() {
    let accepted = BTreeMap::from([(
        "--strategy".to_owned(),
        vec!["ff".to_owned(), "rebase".to_owned()],
    )]);
    let row = "| `rwv sync <source>` | `--strategy ff\\|rebase` (default `ff`) |";
    let errors = value_claim_errors("README.md", "sync", row, &accepted);
    assert!(
        errors.is_empty(),
        "accepted values must not be reported, got:\n{}",
        errors.join("\n")
    );
}

/// A placeholder is the doc naming the argument's shape, not offering values.
#[test]
fn a_placeholder_argument_is_not_a_value_claim() {
    let claims = extract_flag_value_claims("| `--repo <NAME>\\|<PATH>` | selector |");
    assert!(
        claims.is_empty(),
        "a placeholder spec must not be read as values, got: {claims:?}"
    );
}

/// **Non-vacuity pin for both live surfaces.** The two checks above iterate
/// docs and report what they find; finding nothing is indistinguishable from
/// finding nothing wrong.
///
/// This caught a real one: `section_text` originally cut the section at the end
/// of the matched header rather than at the line break, and the odd backtick in
/// `` ### `rwv sync <source>` `` inverted every span's parity for the rest of
/// the section. The cli.md check read zero claims and passed a seeded
/// `--strategy squash`.
#[test]
fn both_surfaces_actually_yield_a_strategy_claim() {
    let cli_md = read_cli_md();
    let cli_claims = extract_flag_value_claims(section_text(&cli_md, "### `rwv sync"));
    assert!(
        cli_claims.iter().any(|(flag, _)| flag == "--strategy"),
        "cli.md's `rwv sync` section must yield a --strategy value claim; \
         a check that reads nothing passes everything. Got: {cli_claims:?}"
    );

    let readme = read_doc("README.md");
    let readme_claims: Vec<_> = readme
        .lines()
        .filter(|l| l.trim_start().starts_with("| `rwv sync <source>`"))
        .flat_map(extract_flag_value_claims)
        .collect();
    assert!(
        readme_claims.iter().any(|(flag, _)| flag == "--strategy"),
        "README's `rwv sync` row must yield a --strategy value claim. \
         Got: {readme_claims:?}"
    );
}

/// **Pinning test for the `Possible values:` parse.** If this stops finding
/// the block, `possible_values_by_flag` returns an empty map, every doc surface
/// is skipped, and both value tests pass while checking nothing — the exact
/// shape of failure this file's own gates were written against.
#[test]
fn possible_values_are_found_for_the_enum_valued_flag() {
    let accepted = possible_values_by_flag(&help_text(&["sync"]));
    assert_eq!(
        accepted.get("--strategy").map(Vec::as_slice),
        Some(["ff".to_owned(), "rebase".to_owned()].as_slice()),
        "clap's `Possible values:` block for --strategy must parse; got: {accepted:?}"
    );
}
