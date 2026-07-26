//! Doc-claims tests for `rwv prime --no-suppress` / `render_overview`.
//!
//! Four classes of check anchored to the committed
//! `docs/reference/prime/overview.md` (rendered from
//! `docs/reference/prime/templates/overview.md.tmpl` by `cargo run --bin
//! generate-explain`):
//!
//! **Class 1 - CLI claim verification.**
//! Scan all inline backtick-quoted `rwv <verb> [--flag...]` occurrences in
//! overview.md (table cells, prose, code fences).  For every invocation that
//! has explicit `--flag` tokens, run `rwv <verb> --help` and assert the flag
//! appears in the help output.  Catches `rwv fetch --locked` class of
//! stale-flag drift (a flag renamed or removed while the doc still lists it).
//!
//! **Class 2 - Verb-shape verification.**
//! For each row of the "Essential commands" / "Sync family" tables, parse the
//! leading `rwv <subcommand-chain>` and assert the subcommand chain is
//! accepted by clap (i.e., `rwv <verb> --help` exits without
//! "unrecognized subcommand").  Catches `rwv workweave PROJECT NAME` class
//! (missing `create` subcommand).
//!
//! **Class 3 - On-disk path shape verification.**
//! Bootstraps a tempdir workspace, creates a project, creates a workweave
//! named `feat`, and asserts the resulting path matches
//! `.workweaves/<project>--<name>/` as claimed in the prime overview.
//! Catches `.workweaves/payments` class (missing project prefix).
//!
//! **Class 4 - JSON-record-shape verification.**
//! For the "Agent integration surfaces" section's claims about which fields
//! `status`/`sync`/`doctor` `--json` records carry, parse the committed
//! schema artifacts (`docs/reference/schemas/<verb>.json`) instead of live
//! output — doctor's ~30 violation kinds are impractical to fabricate on
//! disk one by one, and the schema is generated from the same types that
//! produce the JSON and is diffed for drift in CI. Catches `path` +
//! `absolute_path` (or `kind`) claimed as universal across all three verbs
//! when a verb's records actually vary by kind.

use serde_json::Value;
use std::path::Path;
use std::process;

mod common;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(
        status.success(),
        "git {:?} in {} failed",
        args,
        dir.display()
    );
}

fn init_repo_with_commit(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "--initial-branch=main"], path);
    git(&["config", "user.email", "test@test.com"], path);
    git(&["config", "user.name", "Test"], path);
    std::fs::write(path.join("README"), "init").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
}

/// Minimal workspace: one git repo, one project dir with `rwv.yaml`.
/// Returns workspace root.
fn make_workspace(tmp: &Path, project: &str) -> std::path::PathBuf {
    let ws = tmp.join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();

    let manifest = format!(
        "repositories:\n  github/org/repo:\n    type: git\n    url: file://{repo}\n    version: main\n    role: owned\n",
        repo = repo_path.display()
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();
    ws
}

/// The committed prime overview markdown.  Embedded at compile time so CI
/// catches drift between the rendered artifact and the test suite.
const PRIME_OVERVIEW_MD: &str = include_str!("../docs/reference/prime/overview.md");

/// Committed JSON Schema artifacts backing the "Agent integration surfaces"
/// claims (Class 4).  Same embed-at-compile-time rationale as
/// `PRIME_OVERVIEW_MD`.
const STATUS_SCHEMA_JSON: &str = include_str!("../docs/reference/schemas/status.json");
const SYNC_SCHEMA_JSON: &str = include_str!("../docs/reference/schemas/sync.json");
const DOCTOR_SCHEMA_JSON: &str = include_str!("../docs/reference/schemas/doctor.json");

// ===========================================================================
// Class 1 - CLI claim verification
//
// Source locations in overview.md that carry flags:
//   - Table cells: `rwv prime [--no-suppress]`  => --no-suppress
//   - Table cells: `rwv fetch SOURCE [--frozen]` => --frozen
//   - Table cells: `rwv doctor --locked`         => --locked
//   - Table cells: `rwv sync <source> [--strategy ff|rebase]`
//                                                => --strategy
//   - Table cells: `rwv status [--json]`         => --json
//   - Code fences: bare `rwv <verb> --flag` lines (no placeholders / comments)
//
// Extraction rules for inline backtick spans:
//   1. Find all `...` spans whose content starts with `rwv `.
//   2. Tokenize; collect tokens starting with `--` after stripping `[`, `]`,
//      `\|`, `|` characters that appear in table markdown.
//   3. Skip spans that contain only placeholder `<arg>` tokens and no flags.
// ===========================================================================

/// Collect (verb, flags) pairs from inline backtick spans and code fences.
fn parse_prime_flag_claims(md: &str) -> Vec<(String, Vec<String>)> {
    // Map from verb -> deduplicated flag list.
    let mut map: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();

    // Helper: insert (verb, flag) deduplicating.
    let mut insert = |verb: String, flag: String| {
        let flags = map.entry(verb).or_default();
        if !flags.contains(&flag) {
            flags.push(flag);
        }
    };

    // --- 1. Inline backtick spans (table cells, prose) ----------------------
    let mut rest = md;
    while let Some(start) = rest.find('`') {
        rest = &rest[start + 1..];
        let end = match rest.find('`') {
            Some(e) => e,
            None => break,
        };
        let span = &rest[..end];
        rest = &rest[end + 1..];

        // Only spans that start with `rwv ` (possibly after normalizing).
        if !span.starts_with("rwv ") {
            continue;
        }
        let tokens: Vec<&str> = span.split_whitespace().collect();
        if tokens.len() < 3 {
            // Need at least `rwv <verb> <something>`.
            continue;
        }
        let verb = tokens[1].to_string();

        for tok in &tokens[2..] {
            // Strip bracket/pipe/backslash noise from table markdown syntax.
            let cleaned = tok
                .trim_matches('[')
                .trim_matches(']')
                .trim_start_matches('\\')
                .trim_matches('|');
            if cleaned.starts_with("--") {
                insert(verb.clone(), cleaned.to_string());
            }
        }
    }

    // --- 2. Code fences: bare `rwv <verb> --flag` lines ---------------------
    let mut in_fence = false;
    for line in md.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence || !trimmed.starts_with("rwv ") {
            continue;
        }
        // Skip placeholder lines or comment lines.
        if trimmed.contains('<') || trimmed.contains(" # ") || trimmed.starts_with('#') {
            continue;
        }
        let tokens: Vec<&str> = trimmed.split_whitespace().collect();
        if tokens.len() < 3 {
            continue;
        }
        let verb = tokens[1].to_string();
        for tok in &tokens[2..] {
            let cleaned = tok.trim_end_matches(']').trim_end_matches('|');
            if cleaned.starts_with("--") {
                insert(verb.clone(), cleaned.to_string());
            }
        }
    }

    // Convert map to sorted Vec for stable test output.
    let mut results: Vec<(String, Vec<String>)> = map.into_iter().collect();
    results.sort_by(|a, b| a.0.cmp(&b.0));
    results
}

#[test]
fn class1_prime_flag_claims_exist_in_help() {
    let claims = parse_prime_flag_claims(PRIME_OVERVIEW_MD);

    // Sanity: overview.md documents flags like --no-suppress (prime),
    // --frozen (fetch), --locked (doctor), --json (status), --force (sync).
    // We expect to find at least 3 distinct verbs with flags.
    assert!(
        claims.len() >= 3,
        "Expected at least 3 verbs with flag claims in overview.md, \
         found {}: {:?}\n\
         Check parse_prime_flag_claims if the doc format changed.",
        claims.len(),
        claims.iter().map(|(v, _)| v.as_str()).collect::<Vec<_>>()
    );

    for (verb, flags) in &claims {
        // `rwv <verb> --help` exits 0 and prints help even outside a workspace.
        let output = rwv()
            .args([verb.as_str(), "--help"])
            .output()
            .unwrap_or_else(|e| panic!("Failed to run `rwv {verb} --help`: {e}"));
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        for flag in flags {
            assert!(
                combined.contains(flag.as_str()),
                "Flag `{flag}` claimed for `rwv {verb}` in overview.md is not present \
                 in `rwv {verb} --help` output.\n\
                 This indicates stale documentation - the flag may have been renamed \
                 or removed.\nHelp output:\n{combined}"
            );
        }
    }
}

// ===========================================================================
// Class 2 - Verb-shape verification
//
// For each markdown table row whose first cell starts with `rwv`, parse the
// subcommand chain (words between `rwv` and the first `[`, `<`, `|`, or `--`)
// and assert `rwv <chain> --help` is accepted by clap without
// "unrecognized subcommand".
//
// Catches: `rwv workweave PROJECT NAME` (missing `create` between NAME and
// PROJECT) - would appear as `workweave` in the chain, not `workweave create`.
// ===========================================================================

/// Extract (subcommand_chain) from markdown table rows.
///
/// Scans `| ... |` rows. Finds cells with a backtick span starting `rwv `,
/// extracts the subcommand words (up to first `[`, `<`, flag, or pipe).
fn parse_prime_verb_shapes(md: &str) -> Vec<Vec<String>> {
    let mut results: Vec<Vec<String>> = Vec::new();

    for line in md.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            continue;
        }
        // Check that the line has at least one `rwv ` snippet.
        if !trimmed.contains("rwv ") {
            continue;
        }

        // Extract all backtick spans from the row.
        let mut rest = trimmed;
        while let Some(start) = rest.find('`') {
            rest = &rest[start + 1..];
            let end = match rest.find('`') {
                Some(e) => e,
                None => break,
            };
            let span = &rest[..end];
            rest = &rest[end + 1..];

            if !span.starts_with("rwv ") {
                continue;
            }

            let tokens: Vec<&str> = span.split_whitespace().collect();
            // tokens[0] = "rwv"; collect non-flag, non-placeholder words after.
            let chain: Vec<String> = tokens[1..]
                .iter()
                .take_while(|t| {
                    !t.starts_with("--")
                        && !t.starts_with('<')
                        && !t.starts_with('[')
                        && !t.contains('|')
                        && !t.contains('\\')
                })
                // Skip all-uppercase tokens (POSITIONAL ARG placeholders like PROJECT, NAME, SOURCE).
                .filter(|t| !t.chars().all(|c| c.is_uppercase() || c == '_' || c == '-'))
                .map(|s| s.to_string())
                .collect();

            if !chain.is_empty() && !results.contains(&chain) {
                results.push(chain);
            }
        }
    }
    results
}

#[test]
fn class2_prime_verb_shapes_accepted_by_clap() {
    let shapes = parse_prime_verb_shapes(PRIME_OVERVIEW_MD);

    // Sanity: we should find at least rwv, prime, fetch, activate, workweave ...
    assert!(
        shapes.len() >= 4,
        "Expected at least 4 verb shapes from overview.md tables, found {}",
        shapes.len()
    );

    for chain in &shapes {
        // Build: rwv <subcommand-chain...> --help
        let mut args: Vec<&str> = chain.iter().map(String::as_str).collect();
        args.push("--help");

        let output = rwv()
            .args(&args)
            .output()
            .unwrap_or_else(|e| panic!("Failed to run `rwv {} --help`: {e}", chain.join(" ")));

        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        assert!(
            !combined.contains("unrecognized subcommand"),
            "overview.md documents `rwv {}` but clap reports 'unrecognized subcommand'.\n\
             This may indicate a missing subcommand in the documented chain (e.g. \
             `rwv workweave PROJECT NAME` missing `create`).\n\
             Combined output:\n{combined}",
            chain.join(" ")
        );
        assert!(
            !combined.contains("unexpected argument"),
            "overview.md documents `rwv {}` but clap reports 'unexpected argument'.\n\
             Combined output:\n{combined}",
            chain.join(" ")
        );
    }
}

// ===========================================================================
// Class 3 - On-disk path shape verification
//
// Claim in overview.md (Typical flow + Concepts section):
//   workweaves land at `.workweaves/<project>--<name>/`
//
// Test: bootstrap a workspace for project `my-proj`, create a workweave named
// `feat`, assert path `<weaveroot>/my-proj--feat` exists and that a bare
// `feat` path (without project prefix) does NOT exist.
// ===========================================================================

#[test]
fn class3_workweave_path_shape_matches_prime_claim() {
    // The prime overview states (Concepts section):
    //   "Created with `rwv workweave PROJECT create NAME`"
    //   (path shape: .workweaves/<project>--<name>/)
    //
    // And the Typical flow example:
    //   "# ... edit, test, commit across repos in .workweaves/<project>--<name>/ ..."

    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "my-proj");

    // Redirect workweave output to a controlled directory.
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "my-proj", "create", "feat"])
        .current_dir(&ws)
        .assert()
        .success();

    // The path must be <weaveroot>/my-proj--feat.
    // (project prefix + double-dash separator + workweave name)
    let expected = weaveroot.join("my-proj--feat");
    assert!(
        expected.exists(),
        "overview.md claims workweaves land at `.workweaves/<project>--<name>/` \
         (with the project prefix). Expected path:\n  {}\n  ...does not exist.\n\
         Actual contents of weaveroot ({}):\n  {}",
        expected.display(),
        weaveroot.display(),
        std::fs::read_dir(&weaveroot)
            .map(|it| {
                it.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_else(|_| "(unreadable)".to_string())
    );

    // Negative: a path WITHOUT the project prefix must NOT exist.
    // (Catches the `.workweaves/payments` class of bug where the
    // prefix is absent and the dir is just `<name>`.)
    let wrong = weaveroot.join("feat");
    assert!(
        !wrong.exists(),
        "A workweave directory named `feat` (without the `my-proj--` project prefix) \
         should NOT exist. Found: {}\n\
         This indicates the path shape is wrong per the prime overview's claim.",
        wrong.display()
    );
}

// ===========================================================================
// Class 4 - JSON-record-shape verification
//
// Doc claim (Agent integration surfaces section): `sync` and `doctor`
// records carry `kind`; `status` has a single record shape and carries no
// `kind`. `path` + `absolute_path` are present on every `status` and `sync`
// record; in `doctor` they are per-kind, not universal.
// ===========================================================================

fn parse_schema(json: &str) -> Value {
    serde_json::from_str(json).expect("committed schema artifact should parse as JSON")
}

/// `variant["required"]` as a `Vec<&str>`. Every schemars-derived
/// object/`oneOf` variant in these schemas declares one.
fn required_fields(variant: &Value) -> Vec<&str> {
    variant["required"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a `required` array in {variant}"))
        .iter()
        .map(|v| v.as_str().expect("`required` entries are strings"))
        .collect()
}

#[test]
fn class4_status_records_have_no_kind_field() {
    let schema = parse_schema(STATUS_SCHEMA_JSON);
    let repo_status = &schema["definitions"]["RepoStatus"];

    let required = required_fields(repo_status);
    assert!(
        required.contains(&"path") && required.contains(&"absolute_path"),
        "RepoStatus (the `rwv status --json` per-repo record) should require \
         `path` + `absolute_path` per the Agent integration surfaces claim; \
         required: {required:?}"
    );

    let properties = repo_status["properties"]
        .as_object()
        .expect("RepoStatus should declare `properties`");
    assert!(
        !properties.contains_key("kind"),
        "RepoStatus gained a `kind` field — update the Agent integration \
         surfaces claim that `status` records carry no `kind` (`relation` is \
         status's only discriminant)"
    );
}

#[test]
fn class4_sync_records_always_carry_kind_and_path_identifiers() {
    let schema = parse_schema(SYNC_SCHEMA_JSON);
    let variants = schema["definitions"]["SyncOutcomeOutput"]["oneOf"]
        .as_array()
        .expect("SyncOutcomeOutput should be a `oneOf` of per-kind variants");
    assert!(
        !variants.is_empty(),
        "expected at least one `rwv sync --json` outcome kind"
    );

    for variant in variants {
        let required = required_fields(variant);
        for field in ["kind", "path", "absolute_path"] {
            assert!(
                required.contains(&field),
                "every `rwv sync --json` outcome must require `{field}` per \
                 the Agent integration surfaces claim; variant: {variant}"
            );
        }
    }
}

#[test]
fn class4_doctor_violations_carry_kind_but_path_identifiers_are_per_kind() {
    let schema = parse_schema(DOCTOR_SCHEMA_JSON);
    let variants = schema["definitions"]["ViolationOutput"]["oneOf"]
        .as_array()
        .expect("ViolationOutput should be a `oneOf` of per-kind variants");
    assert!(
        !variants.is_empty(),
        "expected at least one `rwv doctor --json` violation kind"
    );

    for variant in variants {
        assert!(
            required_fields(variant).contains(&"kind"),
            "every doctor violation kind must require `kind`; variant: {variant}"
        );
    }

    // The claim is specifically that `path` + `absolute_path` are NOT
    // universal here (unlike status/sync) — some kinds carry a kind-specific
    // field instead. If every kind ever gained both fields, the doc's claim
    // of heterogeneity would go stale; fail loudly instead of drifting.
    let uniform = variants.iter().all(|v| {
        let required = required_fields(v);
        required.contains(&"path") && required.contains(&"absolute_path")
    });
    assert!(
        !uniform,
        "every doctor violation kind now carries `path` + `absolute_path` — \
         the Agent integration surfaces claim that these are per-kind, not \
         universal, for `doctor` is stale; update overview.md.tmpl"
    );

    // The doc's own examples: a weave-root finding names `root`, a
    // workweave-directory finding names `workweave_dir` — neither `path`
    // nor `absolute_path`.
    let root_finding = variants
        .iter()
        .find(|v| v["properties"]["kind"]["enum"][0] == "weave-root-identity-conflict")
        .expect("weave-root-identity-conflict should be a doctor violation kind");
    assert!(
        required_fields(root_finding).contains(&"root"),
        "weave-root-identity-conflict should require `root`; got: {root_finding}"
    );

    let workweave_dir_finding = variants
        .iter()
        .find(|v| v["properties"]["kind"]["enum"][0] == "workweave-tree-integrity")
        .expect("workweave-tree-integrity should be a doctor violation kind");
    assert!(
        required_fields(workweave_dir_finding).contains(&"workweave_dir"),
        "workweave-tree-integrity should require `workweave_dir`; got: {workweave_dir_finding}"
    );
}
