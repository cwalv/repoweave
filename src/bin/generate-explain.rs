//! `generate-explain` — build-time assembler for `rwv explain` artifacts.
//!
//! Reads hand-written templates from `docs/reference/explain/templates/`,
//! splices in schemars-derived JSON Schemas for `--json`-capable verbs,
//! writes assembled markdown to `docs/reference/explain/<verb>.md`,
//! writes raw schemas to `docs/reference/schemas/<verb>.json`,
//! and writes an `index.md` listing all explainable verbs.
//!
//! Run with `cargo run --bin generate-explain`. CI re-runs the generator and
//! fails on drift via `git diff --exit-code docs/reference/explain/
//! docs/reference/schemas/`.
//!
//! # Link-cleanliness invariant
//!
//! The assembled explain docs are agent-facing CLI reflection — a relative
//! markdown link is not clickable and a rustdoc intra-doc link (`](Self::`,
//! `](crate::`, bare `` [`Ty::Variant`] ``) means nothing to an agent.
//! This file enforces two guarantees at generation time:
//!
//! 1. **Rustdoc-link flattening**: schemars pulls `///` doc-comments verbatim
//!    into JSON Schema `description` fields. Any rustdoc intra-doc link syntax
//!    is rewritten to plain backtick-quoted identifiers before embedding. The
//!    Rust source doc-comments are NOT touched — they remain valid rustdoc.
//!
//! 2. **Relative-link check**: after all assembled `.md` files are written,
//!    every relative markdown link `[text](path)` (non-URL, non-anchor) in
//!    every `.md` file under `docs/` (recursively, excluding template
//!    directories) must resolve to an existing file on disk, each link
//!    resolved against its own file's directory. Rustdoc intra-doc syntax
//!    is additionally rejected in assembled output pages
//!    (`docs/reference/explain/` and `docs/reference/prime/`). Any
//!    unresolvable link or surviving rustdoc syntax is a hard generator
//!    error.

use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use clap::CommandFactory;
use regex::Regex;
use schemars::schema_for;

use repoweave::check::DoctorJsonOutput;
use repoweave::cli::Cli;
use repoweave::explain;
use repoweave::fetch::FetchJsonOutput;
use repoweave::plugins::envelope_vars;
use repoweave::push::{PushJsonOutput, PUSH_SCHEMA_URL};
use repoweave::status::StatusJsonOutput;
use repoweave::sync::{
    auto_relock_commit_message, SyncJsonOutput, SyncToJsonOutput, SYNC_JSON_SCHEMA_URL,
    SYNC_TO_JSON_SCHEMA_URL,
};
use repoweave::update::{UpdateJsonOutput, UPDATE_SCHEMA_URL};
use repoweave::workspace::Resolution;

/// One explainable verb.
struct Verb {
    /// Verb name (file stem, used in markdown filename and dispatch).
    name: &'static str,
    /// One-line description for the index.
    summary: &'static str,
    /// `Some` for `--json`-capable verbs; the closure returns the JSON Schema
    /// as a pretty-printed string. `None` for markdown-only verbs.
    schema: Option<fn() -> String>,
}

fn schema_status() -> String {
    let schema = schema_for!(StatusJsonOutput);
    serde_json::to_string_pretty(&schema).expect("status schema serializes")
}

fn schema_doctor() -> String {
    let schema = schema_for!(DoctorJsonOutput);
    serde_json::to_string_pretty(&schema).expect("doctor schema serializes")
}

/// Every `ViolationOutput` variant's `kind` tag, paired with its full variant
/// schema, walked from the schemars-derived doctor JSON Schema rather than a
/// hand-typed list — so it cannot drift out of sync with the enum.
fn doctor_violation_variants() -> Vec<(String, serde_json::Value)> {
    let schema = schema_for!(DoctorJsonOutput);
    let json = serde_json::to_value(&schema).expect("doctor schema serializes");
    json["definitions"]["ViolationOutput"]["oneOf"]
        .as_array()
        .expect("ViolationOutput schema is a oneOf")
        .iter()
        .map(|variant| {
            let kind = variant["properties"]["kind"]["enum"][0]
                .as_str()
                .expect("each ViolationOutput variant has a kind enum const")
                .to_owned();
            (kind, variant.clone())
        })
        .collect()
}

/// Comma-separated, backtick-quoted, alphabetized list of every
/// `ViolationOutput` `kind` tag.
fn doctor_kind_list_md() -> String {
    let mut kinds: Vec<String> = doctor_violation_variants()
        .into_iter()
        .map(|(kind, _)| kind)
        .collect();
    kinds.sort();
    kinds
        .iter()
        .map(|k| format!("`{k}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Same as [`doctor_kind_list_md`], filtered to variants that carry an
/// additional `sub_kind` field.
fn doctor_subkind_variant_list_md() -> String {
    let mut kinds: Vec<String> = doctor_violation_variants()
        .into_iter()
        .filter(|(_, variant)| variant["properties"].get("sub_kind").is_some())
        .map(|(kind, _)| kind)
        .collect();
    kinds.sort();
    kinds
        .iter()
        .map(|k| format!("`{k}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Resolves a `$ref` (or single-element `allOf: [{$ref}]`, schemars' shape
/// for a field whose type has its own doc comment) to the referenced
/// definition's schema.
fn resolve_ref<'a>(
    doctor_schema: &'a serde_json::Value,
    field: &serde_json::Value,
) -> &'a serde_json::Value {
    let r = field["$ref"]
        .as_str()
        .or_else(|| field["allOf"][0]["$ref"].as_str())
        .expect("field is a $ref or a single-element allOf $ref");
    let name = r.rsplit('/').next().expect("$ref has a trailing component");
    &doctor_schema["definitions"][name]
}

/// Kebab-case tag of one oneOf entry from an externally-tagged
/// (`#[serde(rename_all = "kebab-case")]`, no `tag = ...`) enum — the
/// default serde representation used by sub_kind discriminator enums like
/// `WorkweaveTreeIntegrityKind`. Unit variants serialize as a bare string
/// enum; variants carrying fields serialize as a single-key object whose
/// key is the tag.
fn externally_tagged_variant_tag(variant: &serde_json::Value) -> String {
    if let Some(tag) = variant["enum"][0].as_str() {
        return tag.to_owned();
    }
    variant["required"][0]
        .as_str()
        .expect("externally-tagged variant is a unit string enum or a single-key object keyed by its tag")
        .to_owned()
}

/// Comma-separated, backtick-quoted, alphabetized list of
/// `workweave-tree-integrity`'s `sub_kind` tags, walked from
/// `WorkweaveTreeIntegrityKind`'s schema (reached via the `$ref` on
/// `ViolationOutput::WorkweaveTreeIntegrity::sub_kind`) rather than
/// hand-typed.
fn doctor_workweave_tree_integrity_subkind_list_md() -> String {
    let schema = schema_for!(DoctorJsonOutput);
    let json = serde_json::to_value(&schema).expect("doctor schema serializes");
    let (_, variant) = doctor_violation_variants()
        .into_iter()
        .find(|(kind, _)| kind == "workweave-tree-integrity")
        .expect("ViolationOutput has a workweave-tree-integrity variant");
    let sub_kind_schema = resolve_ref(&json, &variant["properties"]["sub_kind"]);
    let mut tags: Vec<String> = sub_kind_schema["oneOf"]
        .as_array()
        .expect("WorkweaveTreeIntegrityKind schema is a oneOf")
        .iter()
        .map(externally_tagged_variant_tag)
        .collect();
    tags.sort();
    tags.iter()
        .map(|t| format!("`{t}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn schema_sync() -> String {
    let schema = schema_for!(SyncJsonOutput);
    serde_json::to_string_pretty(&schema).expect("sync schema serializes")
}

fn schema_sync_to() -> String {
    let schema = schema_for!(SyncToJsonOutput);
    serde_json::to_string_pretty(&schema).expect("sync-to schema serializes")
}

fn schema_fetch() -> String {
    let schema = schema_for!(FetchJsonOutput);
    serde_json::to_string_pretty(&schema).expect("fetch schema serializes")
}

fn schema_update() -> String {
    let schema = schema_for!(UpdateJsonOutput);
    serde_json::to_string_pretty(&schema).expect("update schema serializes")
}

fn schema_push() -> String {
    let schema = schema_for!(PushJsonOutput);
    serde_json::to_string_pretty(&schema).expect("push schema serializes")
}

fn verbs() -> Vec<Verb> {
    vec![
        Verb {
            name: "status",
            summary: "per-repo workspace state (branch, tip, lock, relation)",
            schema: Some(schema_status),
        },
        Verb {
            name: "doctor",
            summary: "convention-violation checks (orphans, drift, stale locks)",
            schema: Some(schema_doctor),
        },
        Verb {
            name: "sync",
            summary: "reconcile each repo with its locked SHA",
            schema: Some(schema_sync),
        },
        Verb {
            name: "sync-to",
            summary: "advance target workspace to CWD's tip (3-step orchestration: rebase, relock, FF-advance)",
            schema: Some(schema_sync_to),
        },
        Verb {
            name: "push",
            summary: "publish manifest repos then the project repo to shared remotes",
            schema: Some(schema_push),
        },
        Verb {
            name: "fetch",
            summary: "clone or fetch every repo in the active project",
            schema: Some(schema_fetch),
        },
        Verb {
            name: "update",
            summary: "advance the lock to current HEADs",
            schema: Some(schema_update),
        },
        Verb {
            name: "prime",
            summary: "agent-oriented orientation context for the workspace",
            schema: None,
        },
        Verb {
            name: "explain",
            summary: "per-verb JIT reflection (this verb)",
            schema: None,
        },
        Verb {
            name: "workweave",
            summary: "create, delete, or list workweaves for a project",
            schema: None,
        },
        Verb {
            name: "abort",
            summary: "restore CWD workspace to its pre-sync state using savepoint refs",
            schema: None,
        },
        Verb {
            name: "add",
            summary: "clone a repo and register it in the active project manifest",
            schema: None,
        },
        Verb {
            name: "remove",
            summary: "remove a repo from the active project manifest",
            schema: None,
        },
        Verb {
            name: "lock",
            summary: "snapshot current repo HEADs into rwv.lock (pure local; no network)",
            schema: None,
        },
        Verb {
            name: "activate",
            summary: "set the active project, create symlinks, run integration install hooks",
            schema: None,
        },
        Verb {
            name: "materialize",
            summary: "run the integration install hooks for the presented project, without selecting anything",
            schema: None,
        },
        Verb {
            name: "init",
            summary: "create a new project (or adopt an existing repo) and auto-activate it",
            schema: None,
        },
    ]
}

/// Flatten rustdoc intra-doc link syntax that schemars pulls verbatim from
/// `///` doc-comments into JSON Schema `description` fields.
///
/// Rewrites:
/// - `` [`X`](Self::X) `` → `` `X` ``
/// - `` [`Ty::Variant`](crate::path::Ty::Variant) `` → `` `Ty::Variant` ``
/// - `` [`X`](crate::X) `` → `` `X` ``
/// - `` [`Ty::Variant`] `` (bare autolink, no target) → `` `Ty::Variant` ``
/// - `[text](../../docs/...)` (relative path escaping the explain dir) → `text`
/// - `[text](../path)` (relative path, broken cross-reference) → `text`
///
/// Intentionally left untouched:
/// - `$schema` JSON Schema standard URLs (http(s)://json-schema.org/…)
/// - Example repo URLs (`https://github.com/`…)
/// - Anchors (#…)
///
/// Applied to the raw JSON string *before* embedding it in the assembled
/// markdown. The Rust source doc-comments are never modified.
fn flatten_rustdoc_links(json: &str) -> String {
    // Pattern: [`display`](Self::target) or [`display`](crate::target)
    // The backtick-quoted display text may contain `::` separators.
    let bracketed_link = Regex::new(r"\[`([^`]+)`\]\((?:Self|crate)::[^)]*\)").unwrap();
    let out = bracketed_link.replace_all(json, "`$1`");

    // Pattern: bare autolink [`Ty::Variant`] with no explicit target.
    // We match `[`...`]` followed by anything other than `(`, by capturing
    // two cases: followed by end-of-text, or followed by a non-`(` char.
    // Implemented via two passes to avoid lookahead (not supported by `regex`).
    //
    // Pass A: [`X`] at end of string.
    let bare_autolink_end = Regex::new(r"\[`([^`]+)`\]$").unwrap();
    let out = bare_autolink_end.replace_all(&out, "`$1`");
    // Pass B: [`X`] followed by a character that is not `(`.
    // We capture the trailing character and re-emit it.
    let bare_autolink_mid = Regex::new(r"\[`([^`]+)`\]([^(])").unwrap();
    let out = bare_autolink_mid.replace_all(&out, "`$1`$2");

    // Pattern: relative path links that escape out of the explain dir.
    // Matches [text](../../...) and [text](../...) style paths.
    // Leave `#anchor` links and absolute URLs untouched.
    let relative_link = Regex::new(r"\[([^\]]+)\]\(\.\.(?:/[^)]+)?\)").unwrap();
    let out = relative_link.replace_all(&out, "$1");

    out.into_owned()
}

/// Collect all `.md` files under `root` recursively, excluding any path whose
/// canonical prefix matches one of the entries in `exclude_dirs` (compared as
/// canonical absolute paths so symlinks don't confuse the check).
fn collect_md_files(root: &Path, exclude_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_md_files_inner(root, exclude_dirs, &mut out);
    out
}

fn collect_md_files_inner(dir: &Path, exclude_dirs: &[PathBuf], out: &mut Vec<PathBuf>) {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if path.is_dir() {
            if exclude_dirs
                .iter()
                .any(|ex| canonical == *ex || canonical.starts_with(ex))
            {
                continue;
            }
            collect_md_files_inner(&path, exclude_dirs, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("md") {
            out.push(path);
        }
    }
}

/// Verify that every `.md` file under `docs_dir` (recursively, excluding
/// template directories) is free of relative markdown links that don't
/// resolve on disk.
///
/// Additionally, files in the assembled output directories
/// (`docs/reference/explain/` and `docs/reference/prime/`) are checked for
/// rustdoc intra-doc link syntax (`](Self::`, `](crate::`, bare
/// `` [`Ty::Variant`] ``), which means nothing outside rustdoc and must not
/// appear in generated/human-facing output.
///
/// Template directories (`docs/reference/explain/templates/` and
/// `docs/reference/prime/templates/`) are excluded entirely: they contain
/// `{{SCHEMA}}`/`{{MSG:...}}` placeholders and template-relative links that
/// are only meaningful after rendering.
///
/// Relative links are resolved against each file's own directory (not a
/// shared base), so cross-tree links are validated correctly regardless of
/// where in `docs/` the file lives.
///
/// Anchor-only and absolute URLs are skipped (unchanged from prior behavior).
///
/// Returns a list of error messages (one per finding); an empty vec means
/// clean.
fn check_assembled_docs(docs_dir: &Path) -> Vec<String> {
    // Template directories to exclude entirely.
    let mut exclude_dirs: Vec<PathBuf> = Vec::new();
    for tmpl_rel in &["reference/explain/templates", "reference/prime/templates"] {
        let p = docs_dir.join(tmpl_rel);
        let canonical = std::fs::canonicalize(&p).unwrap_or(p);
        exclude_dirs.push(canonical);
    }

    // Assembled-output directories: rustdoc-leak detection applies here.
    let mut assembled_dirs: Vec<PathBuf> = Vec::new();
    for asm_rel in &["reference/explain", "reference/prime"] {
        let p = docs_dir.join(asm_rel);
        let canonical = std::fs::canonicalize(&p).unwrap_or(p);
        assembled_dirs.push(canonical);
    }

    // Matches all markdown links: [text](target)
    let link_re = Regex::new(r"\[([^\]]*)\]\(([^)]+)\)").unwrap();
    // Detects rustdoc intra-doc target: ](Self:: or ](crate::
    let rustdoc_target_re = Regex::new(r"\((?:Self|crate)::").unwrap();
    // Detects bare backtick autolink: [`Ty::Variant`] with no `(` after `]`.
    // Two patterns to avoid lookahead: end-of-line and followed by non-`(`.
    let bare_autolink_eol_re = Regex::new(r"\[`[^`]+`\]\s*$").unwrap();
    let bare_autolink_mid_re = Regex::new(r"\[`[^`]+`\][^(]").unwrap();

    let mut errors: Vec<String> = Vec::new();

    let md_files = collect_md_files(docs_dir, &exclude_dirs);
    if md_files.is_empty() {
        errors.push(format!(
            "link-check: no .md files found under {}",
            docs_dir.display()
        ));
        return errors;
    }

    for md_path in &md_files {
        let content = match std::fs::read_to_string(md_path) {
            Ok(c) => c,
            Err(e) => {
                errors.push(format!(
                    "link-check: could not read {}: {e}",
                    md_path.display()
                ));
                continue;
            }
        };

        // Determine whether this file is in an assembled-output directory.
        let canonical_file = std::fs::canonicalize(md_path).unwrap_or_else(|_| md_path.clone());
        let is_assembled = assembled_dirs
            .iter()
            .any(|ad| canonical_file.starts_with(ad));

        if is_assembled {
            // --- Check for bare autolinks (rustdoc leakage not caught by the
            // link regex, since they have no target paren).
            for line in content.lines() {
                if bare_autolink_eol_re.is_match(line) || bare_autolink_mid_re.is_match(line) {
                    // Find the actual match for the error message.
                    let m = bare_autolink_eol_re
                        .find(line)
                        .or_else(|| bare_autolink_mid_re.find(line))
                        .map(|m| m.as_str())
                        .unwrap_or(line);
                    errors.push(format!(
                        "link-check: {}: rustdoc bare autolink in assembled doc: {}",
                        md_path.display(),
                        m
                    ));
                }
            }
        }

        // Resolve links against the file's own directory, not a shared base.
        let file_dir = md_path
            .parent()
            .expect("md file always has a parent directory");

        for cap in link_re.captures_iter(&content) {
            let target = &cap[2];

            // Skip anchors-only and absolute URLs (http/https/ftp).
            if target.starts_with('#') || target.contains("://") {
                continue;
            }

            // Strip a trailing fragment before resolving.
            let path_part = target.split('#').next().unwrap_or(target);

            if is_assembled {
                // Reject rustdoc intra-doc targets that leaked through:
                // ](Self::...) or ](crate::...).
                if rustdoc_target_re.is_match(&format!("({target}")) {
                    errors.push(format!(
                        "link-check: {}: rustdoc intra-doc link in assembled doc: {}",
                        md_path.display(),
                        &cap[0]
                    ));
                    continue;
                }
            }

            // Resolve the relative path against the file's own directory.
            let resolved = file_dir.join(path_part);
            if !resolved.exists() {
                errors.push(format!(
                    "link-check: {}: unresolvable relative link: [{}]({})",
                    md_path.display(),
                    &cap[1],
                    target
                ));
            }
        }
    }

    errors
}

/// Splice `schema_json` (raw JSON) into `template` by replacing the
/// `{{SCHEMA}}` placeholder with a fenced ```json block.
fn render_template(template: &str, schema_json: Option<&str>) -> String {
    let placeholder = "{{SCHEMA}}";
    match schema_json {
        Some(schema) => {
            let block = format!("```json\n{schema}\n```");
            template.replace(placeholder, &block)
        }
        None => {
            // Markdown-only verbs shouldn't contain the placeholder, but if
            // they do, drop it.
            template.replace(placeholder, "")
        }
    }
}

/// Registry mapping `{{MSG:<key>}}` placeholder keys → the exact string the
/// runtime emits, sourced directly from the code that produces it.
///
/// Populated by [`build_msg_registry`]. Using a `HashMap` here keeps the
/// resolver generic: templates reference keys, not hard-coded strings.
type MsgRegistry = HashMap<&'static str, String>;

/// Build the registry of named runtime strings available for `{{MSG:<key>}}`
/// splicing in explain templates.
///
/// # Single-source contract
///
/// Every value MUST be derived from the function or constant in the production
/// code that emits the string at runtime. Do NOT hand-author the value here —
/// that recreates the drift you're eliminating. For interpolated messages (those
/// that take parameters), call the function with a representative sentinel so
/// the doc form shows the structure (e.g. `<source>`) rather than a real value.
///
/// # Adding a new key
///
/// 1. Export the emitting function/constant from the relevant module.
/// 2. Add an entry below, calling the function (possibly with a sentinel).
/// 3. Add `{{MSG:<key>}}` in the relevant template.
/// 4. Re-run `cargo run --bin generate-explain`.
fn build_msg_registry() -> MsgRegistry {
    let mut m: MsgRegistry = HashMap::new();

    // "auto_relock": the commit message written by the sync engine when it
    // regenerates rwv.lock after a rebase step.  The function interpolates the
    // source workspace name; we call it with the sentinel `"<source>"` so the
    // spliced doc form shows the template structure rather than a literal name.
    // Single-source: `repoweave::sync::auto_relock_commit_message` is the ONE
    // place this string lives — both the runtime and the doc derive from it.
    m.insert("auto_relock", auto_relock_commit_message("<source>"));

    // "doctor_kinds"/"doctor_subkind_variants": the `ViolationOutput` `kind`
    // tag enumeration, walked from the schemars-derived doctor JSON Schema
    // (the same schema spliced into doctor.md via `{{SCHEMA}}`) rather than
    // hand-typed, so both stay in sync with the enum.
    m.insert("doctor_kinds", doctor_kind_list_md());
    m.insert("doctor_subkind_variants", doctor_subkind_variant_list_md());

    // "doctor_workweave_tree_integrity_subkinds": one level down from the
    // two keys above — `workweave-tree-integrity`'s own `sub_kind` tag
    // enumeration, walked the same way through `WorkweaveTreeIntegrityKind`.
    m.insert(
        "doctor_workweave_tree_integrity_subkinds",
        doctor_workweave_tree_integrity_subkind_list_md(),
    );

    m
}

/// Replace every `{{MSG:<key>}}` placeholder in `template` with the
/// corresponding value from `registry`.
///
/// Unknown keys are a hard error at generator time so template drift is caught
/// immediately rather than silently emitting a raw placeholder into the doc.
fn resolve_msg_placeholders(template: &str, registry: &MsgRegistry) -> anyhow::Result<String> {
    // Fast path: no placeholder present.
    if !template.contains("{{MSG:") {
        return Ok(template.to_owned());
    }

    let mut out = template.to_owned();
    // Collect all distinct {{MSG:...}} tokens to substitute.
    let re = regex::Regex::new(r"\{\{MSG:([^}]+)\}\}").unwrap();
    let keys: Vec<String> = re
        .captures_iter(template)
        .map(|c| c[1].to_owned())
        .collect();

    for key in keys {
        let placeholder = format!("{{{{MSG:{key}}}}}");
        let value = registry.get(key.as_str()).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown {{{{MSG:{key}}}}} key in template — \
                 add it to `build_msg_registry()` in generate-explain.rs"
            )
        })?;
        out = out.replace(&placeholder, value);
    }
    Ok(out)
}

fn render_index(verbs: &[Verb]) -> String {
    let mut out = String::new();
    out.push_str("# rwv explain — index\n");
    out.push('\n');
    out.push_str(
        "Per-verb agent-oriented reflection. Each entry below has a markdown bundle \
         describing the verb's purpose, invocation, output shape, exit codes, and \
         examples. JSON-capable verbs additionally embed the JSON Schema for their \
         `--json` output.\n",
    );
    out.push('\n');
    out.push_str("Usage:\n");
    out.push('\n');
    out.push_str("```\n");
    out.push_str("rwv explain <verb>\n");
    out.push_str("```\n");
    out.push('\n');
    out.push_str("## Verbs\n");
    out.push('\n');
    for verb in verbs {
        let json_marker = if verb.schema.is_some() {
            " (`--json` available)"
        } else {
            ""
        };
        out.push_str(&format!(
            "- **{}** — {}{}\n",
            verb.name, verb.summary, json_marker
        ));
    }
    out.push('\n');
    out.push_str(
        "Committed schemas live under `docs/reference/schemas/`. CI fails on drift \
         between Rust types and committed artifacts; do not hand-edit the assembled \
         files — edit `docs/reference/explain/templates/<verb>.md.tmpl` and re-run \
         `cargo run --bin generate-explain`.\n",
    );
    out
}

/// Walk the clap command tree and collect every subcommand path as a
/// space-separated string (e.g. `"workweave log"`, `"setup claude"`).
///
/// Top-level commands with no subcommands of their own produce a single-token
/// path (e.g. `"fetch"`). Commands that exist only as umbrella containers
/// (they have their own subcommands) are included both as a path and as
/// prefixes for their children, so the caller can decide whether to require
/// a cli.md entry for the container itself.
///
/// Depth is bounded by the clap tree; the current tree has at most two
/// levels of nesting (top-level → action).
fn collect_subcommand_paths(cmd: &clap::Command) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    collect_subcommand_paths_inner(cmd, "", &mut paths);
    paths
}

fn collect_subcommand_paths_inner(cmd: &clap::Command, prefix: &str, out: &mut Vec<String>) {
    for sc in cmd.get_subcommands() {
        let name = sc.get_name();
        let path = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix} {name}")
        };
        out.push(path.clone());
        // Recurse into nested subcommands (e.g. workweave → create/delete/list/log).
        collect_subcommand_paths_inner(sc, &path, out);
    }
}

/// Parse the coverage allowlist at `allowlist_path`.
///
/// # File format
///
/// One entry per non-blank, non-comment line:
///
/// ```text
/// <check>:<surface-path>  # <reason>
/// ```
///
/// - `<check>` is `cli-md` or `registry`
/// - `<surface-path>` is the full subcommand path as in the rwv CLI tree
///   (e.g. `workweave log`, `setup`)
/// - `# <reason>` is required prose explaining the omission
///
/// The format is intentionally machine-readable for the checks below but
/// grep-friendly for humans reviewing "what did we skip and why."
///
/// Returns `(cli_md_allowlist, registry_allowlist)` as sets of surface paths.
fn load_coverage_allowlist(
    allowlist_path: &Path,
) -> anyhow::Result<(HashSet<String>, HashSet<String>)> {
    let content = fs::read_to_string(allowlist_path).map_err(|e| {
        anyhow::anyhow!(
            "cannot read coverage allowlist at {}: {e}",
            allowlist_path.display()
        )
    })?;

    let mut cli_md: HashSet<String> = HashSet::new();
    let mut registry: HashSet<String> = HashSet::new();

    for (lineno, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        // Skip blanks and comment lines.
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Strip inline comment.
        let entry = trimmed.split('#').next().unwrap_or(trimmed).trim();
        // Parse `<check>:<surface-path>`.
        let colon = entry.find(':').ok_or_else(|| {
            anyhow::anyhow!(
                "allowlist line {}: expected `<check>:<surface-path>`, got: {:?}",
                lineno + 1,
                line
            )
        })?;
        let check = &entry[..colon];
        let surface = entry[colon + 1..].trim();
        if surface.is_empty() {
            anyhow::bail!(
                "allowlist line {}: surface path is empty in: {:?}",
                lineno + 1,
                line
            );
        }
        // Require every entry to have an inline reason (the '#' comment above).
        // We don't enforce the reason text, only that it is present.
        let has_reason = trimmed.contains('#');
        if !has_reason {
            anyhow::bail!(
                "allowlist line {}: entry has no reason comment (add `  # <reason>`): {:?}",
                lineno + 1,
                line
            );
        }
        match check {
            "cli-md" => {
                cli_md.insert(surface.to_owned());
            }
            "registry" => {
                registry.insert(surface.to_owned());
            }
            other => {
                anyhow::bail!(
                    "allowlist line {}: unknown check type {:?} (expected `cli-md` or `registry`)",
                    lineno + 1,
                    other
                );
            }
        }
    }

    Ok((cli_md, registry))
}

/// Does one backtick-quoted invocation span (e.g. the text between backticks
/// in `` `rwv workweave <project> create <name>` ``) cover the subcommand path
/// given as `components` (e.g. `["workweave", "create"]`)?
///
/// The span is tokenized on whitespace. The first token must be `rwv`. Then
/// each path component must appear in order among the remaining tokens, where
/// `<placeholder>` tokens interleaved between components are skipped: clap
/// allows a parent command's positional arguments before the nested
/// subcommand (`rwv workweave [PROJECT] [COMMAND]`), so the documented
/// invocation legitimately writes `<project>` between `workweave` and
/// `create`. Any other literal token where a component is expected fails the
/// match (so `workweave log` does not match `` `rwv workweave list` `` or a
/// hypothetical `` `rwv workweave log-extra` ``). Tokens after the last
/// matched component (flags, further args) are ignored.
fn span_covers_path(span: &str, components: &[&str]) -> bool {
    let mut tokens = span.split_whitespace();
    if tokens.next() != Some("rwv") {
        return false;
    }
    let mut remaining = components.iter();
    let mut expected = remaining.next();
    for token in tokens {
        let Some(&comp) = expected else {
            // All components matched; trailing args/flags are fine.
            return true;
        };
        if token == comp {
            expected = remaining.next();
        } else if token.starts_with('<') && token.ends_with('>') {
            // A positional placeholder between path components — skip it.
            continue;
        } else {
            // A literal token that is not the expected component: this span
            // documents a different invocation.
            return false;
        }
    }
    expected.is_none()
}

/// Does a heading line cover the subcommand path? A heading covers a path iff
/// any backtick-quoted span within it satisfies [`span_covers_path`].
fn heading_covers_path(line: &str, components: &[&str]) -> bool {
    let mut rest = line;
    while let Some(open) = rest.find('`') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find('`') else {
            break;
        };
        let span = &rest[..close];
        rest = &rest[close + 1..];
        if span_covers_path(span, components) {
            return true;
        }
    }
    false
}

/// Check that every subcommand path in `paths` appears in `cli_md_content`
/// as a heading whose backtick-quoted invocation covers the path.
///
/// # Match rule
///
/// A subcommand path (e.g. `workweave log`) is "covered" iff the cli.md
/// content contains at least one heading line (a line whose first
/// non-whitespace character is `#`) with a backtick-quoted span that starts
/// with `rwv` and contains the path's literal components in order, allowing
/// `<placeholder>` positional tokens interleaved between components — see
/// [`span_covers_path`] for the token-level rule. Examples:
///
/// - `` `rwv fetch <source> [...]` `` covers `fetch`
/// - `` `rwv workweave <project> log [--diff] [--json]` `` covers
///   `workweave log` (the `<project>` positional belongs to the parent
///   command and precedes the action subcommand in the real invocation)
/// - `` `rwv workweave <project> list` `` does NOT cover `workweave log`
///
/// Restricting the match to headings avoids false positives from incidental
/// body-text mentions (the word "log" alone would match dozens of lines).
///
/// Entries listed in `allowlist` are silently skipped.
///
/// Returns error messages (one per uncovered path); empty means clean.
fn check_cli_md_coverage(
    paths: &[String],
    cli_md_content: &str,
    allowlist: &HashSet<String>,
) -> Vec<String> {
    // Pre-collect heading lines for fast scanning.
    let heading_lines: Vec<&str> = cli_md_content
        .lines()
        .filter(|l| l.trim_start().starts_with('#'))
        .collect();

    let mut errors: Vec<String> = Vec::new();
    for path in paths {
        if allowlist.contains(path.as_str()) {
            continue;
        }
        let components: Vec<&str> = path.split(' ').collect();
        let found = heading_lines
            .iter()
            .any(|line| heading_covers_path(line, &components));
        if !found {
            errors.push(format!(
                "coverage-cli-md: `rwv {path}` is absent from docs/reference/cli.md \
                 (add a heading whose invocation contains the components `rwv {path}` \
                 in order — placeholder positionals like `<project>` may sit between \
                 them — or add `cli-md:{path}` to docs/cli-coverage-allowlist.txt \
                 with a reason)"
            ));
        }
    }
    errors
}

/// Check that `verbs` (this file's registry) and `runtime_verbs`
/// (`repoweave::explain::known_verbs()`, the runtime dispatch table in
/// `src/explain.rs`) name exactly the same set of verbs.
///
/// Returns one error per verb present in only one of the two; empty means clean.
fn check_verb_registry_consistency(verbs: &[Verb], runtime_verbs: &[&str]) -> Vec<String> {
    let generator: HashSet<&str> = verbs.iter().map(|v| v.name).collect();
    let runtime: HashSet<&str> = runtime_verbs.iter().copied().collect();
    let mut only_generator: Vec<&str> = generator.difference(&runtime).copied().collect();
    let mut only_runtime: Vec<&str> = runtime.difference(&generator).copied().collect();
    only_generator.sort();
    only_runtime.sort();

    let mut errors: Vec<String> = Vec::new();
    for verb in only_generator {
        errors.push(format!(
            "verb-registry: `{verb}` is in verbs() (src/bin/generate-explain.rs) \
             but not in repoweave::explain::known_verbs() (src/explain.rs)"
        ));
    }
    for verb in only_runtime {
        errors.push(format!(
            "verb-registry: `{verb}` is in repoweave::explain::known_verbs() \
             (src/explain.rs) but not in verbs() (src/bin/generate-explain.rs)"
        ));
    }
    errors
}

/// Check that `base` (the schema-URL string template) appears nowhere in
/// `files` except `src/schema_url.rs`, where the shared `schema_url!` macro
/// spells it once.
fn check_schema_url_scheme(root: &Path, files: &[PathBuf], base: &str) -> Vec<String> {
    let mut errors = Vec::new();
    for path in files {
        if path.file_name() == Some(OsStr::new("schema_url.rs")) {
            continue;
        }
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        let rel = path.strip_prefix(root).unwrap_or(path).display();
        for (n, line) in content.lines().enumerate() {
            if line.contains(base) {
                errors.push(format!(
                    "{rel}:{}: hand-spells the schema-URL base instead of \
                     `crate::schema_url::schema_url!(\"<verb>\")`",
                    n + 1
                ));
            }
        }
    }
    errors
}

/// `base` is derived from an already-committed schema URL const rather than
/// hand-spelled here, so the checker cannot itself become a second place
/// that types the template out.
fn run_schema_url_scheme_check(root: &Path) -> Vec<String> {
    let base = repoweave::fetch::FETCH_SCHEMA_URL
        .strip_suffix("fetch.json")
        .expect("FETCH_SCHEMA_URL ends with fetch.json");
    check_schema_url_scheme(root, &src_rs_files(root), base)
}

/// Check that every top-level verb in `cli_top_level` appears in the
/// `verbs` list (the explain registry).
///
/// Entries in `allowlist` are skipped.
///
/// Returns error messages (one per unregistered verb); empty means clean.
fn check_registry_coverage(
    cli_top_level: &[String],
    verbs: &[Verb],
    allowlist: &HashSet<String>,
) -> Vec<String> {
    let registered: HashSet<&str> = verbs.iter().map(|v| v.name).collect();
    let mut errors: Vec<String> = Vec::new();
    for verb in cli_top_level {
        if allowlist.contains(verb.as_str()) {
            continue;
        }
        if !registered.contains(verb.as_str()) {
            errors.push(format!(
                "coverage-registry: `{verb}` is not registered in verbs() in \
                 src/bin/generate-explain.rs — add a Verb entry, or add \
                 `registry:{verb}` to docs/cli-coverage-allowlist.txt with a reason"
            ));
        }
    }
    errors
}

/// Run both coverage checks (cli-md + registry) and return all errors.
///
/// `root` is the repository root; `verbs` is the already-constructed verb
/// list so the registry check reflects the same list used for generation.
///
/// # Match rules (documented here as the canonical reference)
///
/// **cli.md check**: a subcommand path (e.g. `workweave log`) is covered iff
/// `docs/reference/cli.md` contains a heading line (a line whose first
/// non-whitespace character is `#`) with a backtick-quoted invocation span
/// that starts with `rwv` and contains the path's literal components in
/// order; `<placeholder>` positional tokens interleaved between components
/// are skipped (clap allows a parent command's positionals before the nested
/// subcommand, so `` `rwv workweave <project> log [--diff]` `` covers
/// `workweave log`). Restricting to headings avoids false positives from
/// incidental body text. See [`span_covers_path`] for the token-level rule.
///
/// **Registry check**: a top-level verb is covered iff it appears as a `name`
/// field in the `verbs()` list in this file. Nested subcommands are not
/// separately required in the registry — explain pages are per top-level verb.
fn run_coverage_checks(root: &Path, verbs: &[Verb]) -> anyhow::Result<Vec<String>> {
    let allowlist_path = root.join("docs/cli-coverage-allowlist.txt");
    let (cli_md_allow, registry_allow) = load_coverage_allowlist(&allowlist_path)?;

    let cli_cmd = Cli::command();
    let all_paths = collect_subcommand_paths(&cli_cmd);

    // Top-level verbs: paths without a space (no parent component).
    let top_level: Vec<String> = all_paths
        .iter()
        .filter(|p| !p.contains(' '))
        .cloned()
        .collect();

    let cli_md_path = root.join("docs/reference/cli.md");
    let cli_md_content = fs::read_to_string(&cli_md_path)
        .map_err(|e| anyhow::anyhow!("cannot read cli.md at {}: {e}", cli_md_path.display()))?;

    let mut errors: Vec<String> = Vec::new();
    errors.extend(check_cli_md_coverage(
        &all_paths,
        &cli_md_content,
        &cli_md_allow,
    ));
    errors.extend(check_registry_coverage(&top_level, verbs, &registry_allow));

    Ok(errors)
}

/// Parse the env-input allowlist at `allowlist_path`.
///
/// # File format
///
/// One entry per non-blank, non-comment line:
///
/// ```text
/// env-input:<VAR_NAME>  # <reason>
/// ```
///
/// - `<VAR_NAME>` is the literal variable name as passed to `std::env::var`
/// - `# <reason>` is required prose including a structural trigger for removal
///
/// Returns the set of allowlisted variable names.
fn load_env_input_allowlist(allowlist_path: &Path) -> anyhow::Result<HashSet<String>> {
    let content = fs::read_to_string(allowlist_path).map_err(|e| {
        anyhow::anyhow!(
            "cannot read env-input allowlist at {}: {e}",
            allowlist_path.display()
        )
    })?;

    let mut vars: HashSet<String> = HashSet::new();

    for (lineno, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        // Skip blanks and comment lines.
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Require every entry to have an inline reason comment.
        if !trimmed.contains('#') {
            anyhow::bail!(
                "env-input allowlist line {}: entry has no reason comment (add `  # <reason>`): {:?}",
                lineno + 1,
                line
            );
        }
        // Strip inline comment.
        let entry = trimmed.split('#').next().unwrap_or(trimmed).trim();
        // Parse `env-input:<VAR_NAME>`.
        let colon = entry.find(':').ok_or_else(|| {
            anyhow::anyhow!(
                "env-input allowlist line {}: expected `env-input:<VAR_NAME>`, got: {:?}",
                lineno + 1,
                line
            )
        })?;
        let check = &entry[..colon];
        if check != "env-input" {
            anyhow::bail!(
                "env-input allowlist line {}: unknown check type {:?} (expected `env-input`)",
                lineno + 1,
                check
            );
        }
        let var_name = entry[colon + 1..].trim();
        if var_name.is_empty() {
            anyhow::bail!(
                "env-input allowlist line {}: variable name is empty in: {:?}",
                lineno + 1,
                line
            );
        }
        vars.insert(var_name.to_owned());
    }

    Ok(vars)
}

/// Collect all `.rs` files under `src_dir` recursively.
fn collect_rs_files(src_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_rs_files_inner(src_dir, &mut out);
    out.sort();
    out
}

fn collect_rs_files_inner(dir: &Path, out: &mut Vec<PathBuf>) {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files_inner(&path, out);
        } else if path.extension().and_then(OsStr::to_str) == Some("rs") {
            out.push(path);
        }
    }
}

/// Scan `src_dir` for `std::env::var` and `std::env::var_os` reads in
/// non-test production code, and check each against the allowlist.
///
/// # Scope
///
/// Covers literal string arguments to std::env::var and std::env::var_os.
/// Reads whose argument is a variable or expression (not a string literal)
/// are not matched — such a pattern would itself be a policy violation and
/// would require a separate audit. Inline test modules are dropped by
/// [`before_test_module`], under whatever name they carry: `#[cfg(test)]` is
/// what puts a region outside this gate, not what someone called the module,
/// and `src/git.rs` names two of its own `branch_model_tests` and
/// `derived_content_tests`. Comment lines (leading `//`) are excluded to avoid
/// false positives from doc examples and inline annotations.
///
/// Returns error messages (one per unlisted read); empty means clean.
fn check_env_input_reads(src_dir: &Path, allowlist: &HashSet<String>) -> Vec<String> {
    // Matches std::env::var("VARNAME") and std::env::var_os("VARNAME") in
    // non-comment, non-test source lines.
    let re = Regex::new(r#"std::env::var(?:_os)?\("([^"]+)"\)"#).unwrap();

    let mut errors: Vec<String> = Vec::new();

    for path in collect_rs_files(src_dir) {
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                errors.push(format!("env-input: could not read {}: {e}", path.display()));
                continue;
            }
        };

        let production_content = before_test_module(&content);

        // Scan line by line, skipping comment lines (// and ///) so that doc
        // examples and annotations don't produce false positives.
        for line in production_content.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            for cap in re.captures_iter(line) {
                let var_name = &cap[1];
                if !allowlist.contains(var_name) {
                    errors.push(format!(
                        "env-input: unlisted std::env::var read of {:?} in {} \
                         (add `env-input:{var_name}` to docs/env-input-allowlist.txt \
                         with a reason, or remove the env read if it violates the \
                         policy: argv addresses; env vars are handoff surfaces, not inputs)",
                        var_name,
                        path.display()
                    ));
                }
            }
        }
    }

    errors
}

/// Run the env-input inventory check and return all errors.
///
/// Reads `docs/env-input-allowlist.txt` relative to `root`, scans `src/`
/// for `std::env::var`/`var_os` reads in production code, and fails on
/// any read not recorded in the allowlist.
fn run_env_input_check(root: &Path) -> anyhow::Result<Vec<String>> {
    let allowlist_path = root.join("docs/env-input-allowlist.txt");
    let allowlist = load_env_input_allowlist(&allowlist_path)?;
    let src_dir = root.join("src");
    Ok(check_env_input_reads(&src_dir, &allowlist))
}

/// Check that nothing outside `crate::git` reaches around the `Vcs` seam.
///
/// The backend type itself is closed by the compiler — it is private to
/// `src/git.rs`, so `error[E0603]` answers "did anyone name it". Two ways past
/// the seam survive that, because neither names a type:
///
/// 1. **Calling the constructor at a call site.** `git_vcs()` is `pub` because
///    `tests/` needs a concrete backend, and `pub` means a verb could mint one
///    instead of accepting a handle — which dispatches correctly and can never
///    be handed a double. It belongs only where a backend is *resolved*:
///    `src/vcs.rs`, whose two named resolvers say why each one cannot resolve
///    from a manifest entry.
/// 2. **Spawning git from scratch.** `Command::new("git")` bypasses both the
///    trait and `git_command`'s environment scrubbing.
/// 3. **Building a path out of git's on-disk layout.** `dir.join(".git")`
///    needs no git type and spawns no process, so neither of the above sees
///    it. A caller that constructs a store path has decided where git keeps
///    one, which is `src/git.rs`'s to decide — `store_path_in` is the
///    construction and `git_dir_link` the resolution.
///
/// Rule 2 is belt to the compiler's braces: `git_command` is private to
/// `src/git.rs`, so assembling git argv elsewhere is `error[E0603]` before
/// this runs. Rule 3 has no such backstop, because a `&str` that happens to
/// spell git's directory is not a type anything can make private.
///
/// **Residue for rule 3**: the needles read a literal used as a *path
/// component* — `join`, `push`, or an equality test against a name. A
/// `.git` reached some other way (`format!("{root}/.git")`, a `PathBuf` built
/// component-wise, a constant defined elsewhere) is invisible. The exclusions
/// are deliberate and measured, not incidental: `strip_suffix(".git")` and
/// `trim_end_matches(".git")` trim a *clone URL* at four production sites,
/// which asks the filesystem nothing.
///
/// Test modules are out of scope for all three: a `#[cfg(test)]` module is
/// free to build a concrete git backend, which is what `git_vcs` is `pub` for.
/// The boundary is [`before_test_module`] — the first `#[cfg(test)]` that
/// actually opens a module, under any name — not `mod tests` by name.
///
/// Returns error messages (one per bypass); empty means clean.
fn check_vcs_seam_bypasses(src_dir: &Path) -> Vec<String> {
    // Written as patterns rather than literals so this file stays in its own
    // scope: a plain `"Command::new(\"git\")"` needle would match the line
    // that defines it.
    let spawn = Regex::new(r#"Command::new\("git"\)"#).unwrap();
    let mint = Regex::new(r"git_vcs\(").unwrap();
    let layout = Regex::new(r#"(join|push)\("\.git["/]|[=!]= *"\.git"|"\.git" *[=!]="#).unwrap();

    let mut errors: Vec<String> = Vec::new();

    for path in collect_rs_files(src_dir) {
        let rel = path
            .strip_prefix(src_dir)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if rel == "git.rs" {
            continue;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                errors.push(format!("vcs-seam: could not read {}: {e}", path.display()));
                continue;
            }
        };

        for (n, line) in before_test_module(&content).lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if spawn.is_match(line) {
                errors.push(format!(
                    "vcs-seam: src/{rel}:{} spawns git directly; go through the `Vcs` \
                     handle the frame was given, or move the call into src/git.rs",
                    n + 1
                ));
            }
            if rel != "vcs.rs" && mint.is_match(line) {
                errors.push(format!(
                    "vcs-seam: src/{rel}:{} mints a git backend; take a `&dyn Vcs` \
                     parameter instead, or resolve it in src/vcs.rs beside a named \
                     reason it cannot come from a manifest entry",
                    n + 1
                ));
            }
            if layout.is_match(line) {
                errors.push(format!(
                    "vcs-seam: src/{rel}:{} builds a path out of git's on-disk \
                     layout; call `git::store_path_in` for where a store sits, \
                     `git::git_dir_link` to follow a checkout to its git dir, or \
                     name the entry with `git::GIT_DIR_ENTRY_NAME`",
                    n + 1
                ));
            }
        }
    }

    errors
}

/// Check that every variable name emitted by [`repoweave::plugins::envelope_vars`]
/// is documented in `docs/reference/plugin-protocol.md`.
///
/// The check calls `envelope_vars` with a fully-populated [`Resolution`] so that
/// every possible variable (including `RWV_WORKWEAVE`, which is only set when a
/// workweave is resolved) is exercised. The set of emitted names is the single
/// source of truth; no source-text grepping is involved.
///
/// Each name must appear as a backtick-quoted table cell (`\`VAR_NAME\``) in the
/// envelope table under "Context envelope" in the plugin-protocol reference,
/// which is the canonical documentation surface for the wire contract. The
/// External commands section of `docs/reference/cli.md` names the variables in
/// passing and links here; it is orientation, not the contract, and pointing
/// this check at it once cost a doc edit a false failure.
///
/// Returns error messages (one per undocumented variable); empty means clean.
fn check_envelope_output_documented(protocol_md_content: &str) -> Vec<String> {
    // Build a fully-populated Resolution so every conditional branch in
    // envelope_vars() fires and we get the complete set of variable names.
    let full_resolution = Resolution {
        workspace: std::path::PathBuf::from("/sentinel/workspace"),
        workweave: Some("sentinel-project--sentinel-ww".to_owned()),
        project: "sentinel-project".to_owned(),
    };

    let vars = envelope_vars(Some(&full_resolution));
    let mut errors: Vec<String> = Vec::new();

    for (name, _) in &vars {
        // The envelope table rows look like: | `RWV_VERSION` | ... |
        let needle = format!("`{name}`");
        if !protocol_md_content.contains(&needle) {
            errors.push(format!(
                "envelope-output: `{name}` is set on every plugin spawn by \
                 `envelope_vars()` in src/plugins.rs but is not documented in \
                 docs/reference/plugin-protocol.md — add a row for `{name}` to \
                 the Context envelope table in that file"
            ));
        }
    }

    errors
}

/// Run the envelope-output documentation coverage check.
///
/// Calls `envelope_vars` with a fully-populated `Resolution` to obtain the
/// complete set of emitted variable names, then checks each against
/// `docs/reference/plugin-protocol.md`. Fails if any emitted name is
/// undocumented.
fn run_envelope_output_check(root: &Path) -> anyhow::Result<Vec<String>> {
    let protocol_md_path = root.join("docs/reference/plugin-protocol.md");
    let protocol_md_content = fs::read_to_string(&protocol_md_path).map_err(|e| {
        anyhow::anyhow!(
            "cannot read plugin-protocol.md at {}: {e}",
            protocol_md_path.display()
        )
    })?;
    Ok(check_envelope_output_documented(&protocol_md_content))
}

/// Match a tracker ID (`fo-<slug>`, optionally dotted) at a word boundary.
///
/// Deliberately hand-rolled rather than regex: the generator has no regex
/// dependency, and the shape is narrow enough to scan directly.
fn find_tracker_id(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while let Some(off) = line[i..].find("fo-") {
        let start = i + off;
        let prev_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let mut end = start + 3;
        while end < bytes.len() && (bytes[end].is_ascii_lowercase() || bytes[end].is_ascii_digit())
        {
            end += 1;
        }
        let slug_len = end - start - 3;
        if prev_ok && (4..=8).contains(&slug_len) {
            // Include a trailing `.N` sub-ID so the reported token matches
            // what the author wrote.
            let mut tail = end;
            if tail < bytes.len() && bytes[tail] == b'.' {
                let mut d = tail + 1;
                while d < bytes.len() && bytes[d].is_ascii_digit() {
                    d += 1;
                }
                if d > tail + 1 {
                    tail = d;
                }
            }
            return Some(line[start..tail].to_owned());
        }
        i = start + 3;
    }
    None
}

/// Collect every file the src/+docs/ textual gates scan: all of `src/`, all
/// `.md` under `docs/`, plus `docs/env-input-allowlist.txt` (the one
/// non-`.md` doc file these gates care about). Feeds `src_rs_files`, the
/// scope of `check_doc_citations` and `check_doc_symbol_refs` — both stay
/// `tests/`-exempt, per the citation rule's own carve-out for a test naming
/// the regression it pins and describing the scenario it reproduces.
fn src_and_docs_files(root: &Path) -> Vec<PathBuf> {
    let mut files = collect_rs_files(&root.join("src"));
    files.extend(collect_md_files(&root.join("docs"), &[]));
    files.push(root.join("docs/env-input-allowlist.txt"));
    files
}

/// `src_and_docs_files` plus every `.rs` file under `tests/`.
///
/// Scope for `check_no_tracker_ids` and `check_no_consumer_vocabulary` only —
/// the citation and symbol gates stay on the narrower `src_and_docs_files`.
/// A tracker ID or a consumer word is a dead end for a standalone cloner
/// wherever it sits, tests included, so unlike the citation carve-out these
/// two gates do not exempt `tests/`.
fn src_docs_and_tests_files(root: &Path) -> Vec<PathBuf> {
    let mut files = src_and_docs_files(root);
    files.extend(collect_rs_files(&root.join("tests")));
    files
}

/// `.md` files directly in `root`, not recursing into subdirectories —
/// `CLAUDE.md`, `README.md`, and any sibling, `docs/` and `tests/` each have
/// their own collector already.
fn root_level_md_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("md") {
            out.push(path);
        }
    }
    out
}

/// `.rs` files directly in `root` — `build.rs` foremost, `src/` and `tests/`
/// each have their own collector already.
fn root_level_rs_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    out
}

/// Scan `src/`, `docs/`, `tests/`, and root-level `.md` and `.rs` files for
/// tracker IDs in comments, error strings, and published prose.
///
/// Code is ground truth and architecture docs carry rationale; a tracker ID
/// in any of these surfaces points a reader at something they cannot open,
/// and it reaches users directly through `rwv explain` (whose pages are
/// `include_str!`'d from `docs/reference/explain/`) and through
/// `anyhow::bail!` text. `tests/` gets no carve-out: a test comment may name
/// the regression it pins in its own words, but not by an ID a standalone
/// cloner cannot look up — that provenance is what the commit that added the
/// test already carries. Root-level files (`CLAUDE.md`, `build.rs`) get no
/// carve-out either: `CLAUDE.md` is the file stating this ban, and the reader
/// most likely to imitate a tracker ID is one who just read it there;
/// `build.rs` is ordinary Rust that happens to sit outside `src/`, not a
/// different kind of surface.
fn check_no_tracker_ids(root: &Path) -> Vec<String> {
    let mut errors = Vec::new();
    let mut files = src_docs_and_tests_files(root);
    files.extend(root_level_md_files(root));
    files.extend(root_level_rs_files(root));
    if files.is_empty() {
        return vec![format!(
            "tracker-ID check: no source/doc/test files found under {}",
            root.display()
        )];
    }
    for path in files {
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let rel = path.strip_prefix(root).unwrap_or(&path).display();
        for (n, line) in content.lines().enumerate() {
            if let Some(id) = find_tracker_id(line) {
                errors.push(format!("{rel}:{}: tracker ID `{id}`", n + 1));
            }
        }
    }
    errors
}

/// File extensions that make a path token in a comment a *document* citation.
///
/// Deliberately excludes `.rs` and `.json`. A comment naming a sibling module
/// (`integrations/cargo_workspace.rs`) writes a path that is meaningful from
/// the reader's position but does not resolve from the repo root, and
/// reporting those would fail this gate on correct code.
const DOC_PATH_EXTENSIONS: &[&str] = &["md", "txt", "rst", "adoc", "tmpl"];

/// Words that state nothing on their own. A comment built from these and a
/// path and nothing else is pointing at a document instead of carrying the
/// invariant itself.
const POINTER_FILLER: &[&str] = &[
    "see",
    "also",
    "cf",
    "per",
    "and",
    "or",
    "for",
    "the",
    "a",
    "an",
    "this",
    "that",
    "these",
    "in",
    "at",
    "on",
    "of",
    "to",
    "from",
    "further",
    "more",
    "detail",
    "details",
    "doc",
    "docs",
    "documented",
    "documentation",
    "reference",
    "references",
    "ref",
    "refs",
    "full",
    "why",
];

/// Byte offset of the `//` that opens a comment on `line`.
///
/// The scan tracks `"` (honouring backslash escapes) so the `//` inside a URL
/// string literal is not read as a comment start. Shared by the two gates that
/// split a line into code and comment, so neither can drift from the other on
/// where that boundary sits.
fn comment_start(line: &str) -> Option<usize> {
    let b = line.as_bytes();
    let mut in_string = false;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' if in_string => i += 1,
            b'"' => in_string = !in_string,
            b'/' if !in_string && b.get(i + 1) == Some(&b'/') => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// The comment text on `line`, and whether the line is comment-only.
///
/// Leading `/` and `!` are stripped, so `///` and `//!` yield the same text as
/// `//`.
fn comment_on_line(line: &str) -> Option<(bool, &str)> {
    let at = comment_start(line)?;
    let text = line[at + 2..].trim_start_matches(['/', '!']).trim();
    Some((line[..at].trim().is_empty(), text))
}

/// The code on `line` — everything before a comment opens, string literals
/// kept.
fn code_on_line(line: &str) -> &str {
    &line[..comment_start(line).unwrap_or(line.len())]
}

/// One run of consecutive comment-only lines, or one trailing comment on a
/// code line, as `(line number, text)` pairs.
///
/// The block, not the line, is what the bare-pointer clause judges: a comment
/// that states its invariant and then points at a joint document is a legal
/// trailing pointer, and only the whole block shows that it did state it.
struct CommentBlock {
    lines: Vec<(usize, String)>,
}

impl CommentBlock {
    fn text(&self) -> String {
        self.lines
            .iter()
            .map(|(_, t)| t.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn comment_blocks(content: &str) -> Vec<CommentBlock> {
    let mut blocks = Vec::new();
    let mut open: Vec<(usize, String)> = Vec::new();
    let close = |open: &mut Vec<(usize, String)>, blocks: &mut Vec<CommentBlock>| {
        if !open.is_empty() {
            blocks.push(CommentBlock {
                lines: std::mem::take(open),
            });
        }
    };
    for (n, line) in content.lines().enumerate() {
        match comment_on_line(line) {
            Some((true, text)) => open.push((n + 1, text.to_owned())),
            Some((false, text)) => {
                close(&mut open, &mut blocks);
                blocks.push(CommentBlock {
                    lines: vec![(n + 1, text.to_owned())],
                });
            }
            None => close(&mut open, &mut blocks),
        }
    }
    close(&mut open, &mut blocks);
    blocks
}

/// Document-citation tokens in `text`: a run of path characters whose last
/// component is `<stem>.<ext>` with `ext` in `DOC_PATH_EXTENSIONS` and `stem`
/// non-empty.
///
/// A `/` is **not** required. A bare filename — `clone-topology.md` — cites a
/// document just as a path does; the reader simply has less to go on. Treating
/// only slashed tokens as citations is what let a bare filename sit
/// unexamined, so the distinction the caller draws is over *where the token
/// may resolve*, not over whether it is a citation at all.
///
/// The non-empty stem is what keeps a bare extension out: prose naming `.md`
/// as a file type is not naming a file.
///
/// Markdown and rustdoc link punctuation is outside the character run, so
/// ``[clone-topology](../../docs/explanation/joints/clone-topology.md)`` and
/// ``[`docs/explanation/joints/clone-topology.md`]`` both yield the path
/// alone. A path wrapped across two comment lines is not seen — the line break
/// splits the run — which is a miss, not a false report.
fn doc_path_tokens(text: &str) -> Vec<&str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-')))
        .map(|run| run.trim_end_matches('.'))
        .filter(|tok| {
            tok.rsplit('/')
                .next()
                .and_then(|last| last.rsplit_once('.'))
                .is_some_and(|(stem, ext)| !stem.is_empty() && DOC_PATH_EXTENSIONS.contains(&ext))
        })
        .collect()
}

/// The last `/`-separated component of `token`.
fn token_filename(token: &str) -> &str {
    token.rsplit('/').next().unwrap_or(token)
}

/// True if `token` resolves against the **repo root**, one of the two bases a
/// citation may be written from.
///
/// A leading `../` run is dropped first. A rustdoc link is written relative to
/// the generated HTML tree, not the repo root, and the question the rule asks
/// is whether a cloner holds the file — not whether the prefix would resolve
/// from `root`. Dropping the prefix cannot admit a genuine escape: the
/// remainder still has to name something that exists here.
fn resolves_from_root(root: &Path, token: &str) -> bool {
    let mut rest = token;
    while let Some(next) = rest.strip_prefix("../").or_else(|| rest.strip_prefix("./")) {
        rest = next;
    }
    !rest.is_empty() && root.join(rest).exists()
}

/// True if `token` resolves against **the citing file's own directory**, the
/// other base a citation may be written from.
///
/// `..` is walked lexically rather than by touching the filesystem, and a run
/// that climbs above `root` fails instead of resolving. A path that leaves the
/// repository is the case this rule exists to catch, and letting
/// `<root>/src/../../projects/…` be answered by the developer's own
/// surroundings would hand exactly that case a pass on the one machine where
/// nobody needs the check.
fn resolves_from_file_dir(root: &Path, dir: &Path, token: &str) -> bool {
    let Ok(rel) = dir.strip_prefix(root) else {
        return false;
    };
    let mut parts: Vec<&OsStr> = rel.iter().collect();
    for component in token.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return false;
                }
            }
            name => parts.push(OsStr::new(name)),
        }
    }
    if parts.is_empty() {
        return false;
    }
    let mut resolved = root.to_path_buf();
    for part in parts {
        resolved.push(part);
    }
    resolved.exists()
}

/// True if the comment says nothing beyond its references — every word left
/// after removing the path tokens is `POINTER_FILLER`.
fn is_bare_pointer(block_text: &str, tokens: &[&str]) -> bool {
    let mut residue = block_text.to_ascii_lowercase();
    for token in tokens {
        residue = residue.replace(&token.to_ascii_lowercase(), " ");
    }
    residue
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .all(|w| POINTER_FILLER.contains(&w))
}

/// Content up to the **first** `#[cfg(test)]` that is followed by a line break
/// and an inline module body — the region every gate here treats as test-only.
///
/// Two of those words are deviations from the obvious matcher, and dropping
/// either silently returns a large span of test content to every caller:
///
/// - **first**, not last. A comment writing the attribute in prose is a later
///   occurrence than the real one, and this file carries several such
///   mentions, hundreds of lines below its own test module.
/// - a **line break** after the attribute. It is what separates the real
///   attribute from a prose mention, which is written inline.
///
/// The module's *name* is not part of the boundary: `mod branch_model_tests`
/// ends the region exactly as `mod tests` does. A boundary that recognised one
/// name decided scope by what someone called a module, which is the defect
/// these gates exist to catch, one level up.
fn before_test_module(content: &str) -> &str {
    let marker = "#[cfg(test)]";
    let mut from = 0;
    while let Some(pos) = content[from..].find(marker) {
        let at = from + pos;
        let after = content[at + marker.len()..].trim_start_matches([' ', '\t', '\r']);
        if after.starts_with('\n')
            && opens_module_body(after.trim_start_matches([' ', '\t', '\r', '\n']))
        {
            return &content[..at];
        }
        from = at + marker.len();
    }
    content
}

/// True if `text` opens an inline module body, under any name and any
/// visibility.
///
/// The brace is what separates a region from a declaration. `src/vcs.rs` opens
/// with `#[cfg(test)] pub(crate) mod testing;`, which gates a whole other file
/// rather than starting a test region — read as a boundary it puts every line
/// of that file below it outside all five gates that call
/// [`before_test_module`].
fn opens_module_body(text: &str) -> bool {
    let mut words = text.split_whitespace().skip_while(|w| w.starts_with("pub"));
    if words.next() != Some("mod") {
        return false;
    }
    let Some(word) = words.next() else {
        return false;
    };
    if !word.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        return false;
    }
    match word
        .trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '_')
        .chars()
        .next()
    {
        Some(c) => c == '{',
        None => words.next().is_some_and(|w| w.starts_with('{')),
    }
}

/// True if `line` is the inline escape-hatch annotation, with a reason.
fn is_local_ref_hatch(line: &str) -> bool {
    line.strip_prefix("weave-local-ref:")
        .is_some_and(|reason| !reason.trim().is_empty())
}

/// Every document filename this repository's own code **operates on** — the
/// last component of each path-shaped token appearing in non-comment `src/`
/// text.
///
/// `CLAUDE.md` already exempts a path a program acts on from the citation
/// rule: `include_str!("../docs/reference/explain/index.md")` is a program
/// reading a file, not a comment citing a document. This set carries that
/// exemption across to the comment that *describes* the same operation —
/// `writes an `index.md` listing all explainable verbs` names the artifact the
/// line below it produces, and asking a reader to "follow" it is a category
/// error.
///
/// Inline test modules are dropped, and that is load-bearing rather than
/// tidiness — the same reason `src_code_identifiers` drops them. A fixture is
/// free to write any filename it likes, so a fixture that counts as evidence
/// would let this gate vouch for the very citation it is meant to reject: this
/// file's own test module writes docs/explanation/joints/clone-topology.md
/// into a temp tree, and counting that filename would have exempted the two
/// live bare citations of it elsewhere in `src/`.
///
/// The limit worth naming: an author can defeat the bare-filename clause by
/// making the program handle that filename somewhere. That is a real hole and
/// a small one — it takes a code change, not a comment, and a program that
/// genuinely operates on the file is the case being exempted.
fn src_code_doc_filenames(root: &Path) -> HashSet<String> {
    let mut names = HashSet::new();
    for path in src_rs_files(root) {
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        for line in before_test_module(&content).lines() {
            for token in doc_path_tokens(code_on_line(line)) {
                names.insert(token_filename(token).to_owned());
            }
        }
    }
    names
}

/// Enforce the **path-resolution clause** of `CLAUDE.md`'s "Comments do not
/// cite trackers or documents", over comments in `src/`:
///
/// - a document citation in a comment must resolve against one of the two
///   bases a reader can be expected to try: the **repo root**, or the
///   **directory of the citing file**. A citation that resolves under neither
///   is unfollowable from a clone, and it rots invisibly, because nothing can
///   check a reference that was never expected to resolve;
/// - a comment whose *entire* content is the reference is a violation even
///   when it resolves — it points instead of stating. A resolving path is
///   legal as a trailing pointer, after the comment has said the thing.
///
/// A site may keep a path that leaves this repository by annotating the line
/// above it `weave-local-ref: <reason>`. The hatch suppresses the resolution
/// clause only; it is not a way to keep a comment that is nothing but a
/// pointer, and there is no allowlist file.
///
/// # Bare filenames are citations
///
/// A token needs no `/` to be a citation. `docs/explanation/joints/clone-topology.md`
/// and that filename alone point at the same document; the second just gives
/// the reader less. Existing *somewhere* in the repo is deliberately **not** a
/// passing condition — that would make the gate answer a question ("is there a
/// file by this name?") the reader cannot ask, since the reader has a comment
/// and no index. Both bases are positional, and a citation that matches
/// neither is one the reader must go hunting for.
///
/// Two limits keep this from firing on text that cites nothing:
///
/// - a bare filename counts only for `.md`, this repository's document format
///   and the only one `docs/` and mdBook use. Written as a path, every
///   `DOC_PATH_EXTENSIONS` suffix still counts; the narrowing is for the
///   filename-alone case, where prose naming an incidental `notes.txt` is far
///   likelier than a citation;
/// - a filename the code itself operates on (`src_code_doc_filenames`) is
///   exempt, and a bare filename is also satisfied by a resolving path to the
///   same filename **in the same comment block** — which is what a markdown
///   link written `[name](path)` already gives the reader, and what the
///   paragraph above does for its own example. Block-level, not file-level: a
///   resolving path elsewhere in the file is not something a reader of this
///   comment has.
///
/// # This gate is not the whole rule
///
/// `CLAUDE.md` bans three shapes in comments. The tracker-ID shape is
/// `check_no_tracker_ids`. The third — a **section pointer** into a design
/// document, a document name followed by a section token — is still not
/// mechanised *as such*. What changed is the size of the gap, not its
/// existence: a section pointer written against a bare filename is now caught,
/// because the filename does not resolve. A section pointer written against a
/// path that *does* resolve is not caught, and neither is one against a
/// document named without a file extension. Reading a green gate as "this tree
/// has no section pointers" is still wrong.
///
/// # Inline test modules are in scope, and this file alone is not
///
/// A `#[cfg(test)]` comment is still a comment someone lands on, and it asks
/// the reader the same question. Stopping at the test boundary left four
/// non-resolving citations standing in `src/git.rs`, `src/sync.rs` and
/// `src/vcs.rs`, three of them naming a document this repository has never
/// contained.
///
/// This file keeps the exclusion, and the reason does not generalise to any
/// other: its fixtures *are* comment text, held in string literals that
/// `comment_blocks` cannot tell from comments. A seeded-failure test for a
/// comment gate has to contain a comment this gate reports, so scanning them
/// makes every anti-vacuity pin a finding against itself — sixteen, measured.
/// The exemption is keyed through `file!()` rather than spelled, so renaming
/// the gate carries it rather than silently widening the scope.
fn check_doc_citations(root: &Path, files: &[PathBuf], operated: &HashSet<String>) -> Vec<String> {
    let own_fixtures = Path::new(file!()).file_name();
    let mut errors = Vec::new();
    for path in files {
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        let rel = path.strip_prefix(root).unwrap_or(path).display();
        let dir = path.parent().unwrap_or(root);
        let scanned = if path.file_name() == own_fixtures {
            before_test_module(&content)
        } else {
            content.as_str()
        };
        for block in comment_blocks(scanned) {
            let joined = block.text();
            let block_tokens = doc_path_tokens(&joined);
            if block_tokens.is_empty() {
                continue;
            }
            // A bare filename the block itself also writes as a resolving path
            // is followable from the comment the reader is holding.
            let shown_as_path: HashSet<&str> = block_tokens
                .iter()
                .filter(|tok| {
                    tok.contains('/')
                        && (resolves_from_root(root, tok) || resolves_from_file_dir(root, dir, tok))
                })
                .map(|tok| token_filename(tok))
                .collect();
            for (i, (n, text)) in block.lines.iter().enumerate() {
                if i > 0 && is_local_ref_hatch(&block.lines[i - 1].1) {
                    continue;
                }
                for token in doc_path_tokens(text) {
                    if !token.contains('/')
                        && (!token.ends_with(".md")
                            || operated.contains(token)
                            || shown_as_path.contains(token))
                    {
                        continue;
                    }
                    if !resolves_from_root(root, token) && !resolves_from_file_dir(root, dir, token)
                    {
                        errors.push(format!(
                            "{rel}:{n}: `{token}` resolves neither from the repo root nor from \
                             `{}` — a reader has nothing to open",
                            dir.strip_prefix(root).unwrap_or(dir).display()
                        ));
                    }
                }
            }
            if is_bare_pointer(&joined, &block_tokens) {
                errors.push(format!(
                    "{rel}:{}: comment is nothing but a reference — it points instead of stating",
                    block.lines[0].0
                ));
            }
        }
    }
    errors
}

/// The `.rs` half of `src_and_docs_files` — the scope of the two gates that
/// read comments rather than whole files.
///
/// Both rules govern comments under `src/`, so the `.md` files that shared
/// scope also yields are filtered out; `tests/` is exempt for free, since that
/// helper never collects it.
fn src_rs_files(root: &Path) -> Vec<PathBuf> {
    src_and_docs_files(root)
        .into_iter()
        .filter(|p| p.extension() == Some(OsStr::new("rs")))
        .collect()
}

fn run_doc_citation_check(root: &Path) -> Vec<String> {
    check_doc_citations(root, &src_rs_files(root), &src_code_doc_filenames(root))
}

/// Every identifier that appears in `src/` outside a comment.
///
/// This is what a comment's claims are checked against, and it is deliberately
/// weaker than "is declared here". It asks only whether a name occurs in code
/// at all, so a method reached through a derive, a macro expansion or a trait
/// default counts without this file having to resolve Rust items. Only a name
/// that appears in no code anywhere is a phantom.
///
/// String literals are kept. A name registered as data rather than as an item
/// — an environment variable, a hook event, a subcommand — is still something
/// this repository defines and a comment may name it; excluding literals
/// turned twenty such names into findings when it was measured.
///
/// Inline test modules are dropped, and that is load-bearing rather than
/// tidiness. A `#[cfg(test)]` fixture is free to spell a deleted symbol — the
/// tests below this gate quote one on purpose — and a fixture that counts as
/// evidence makes the gate vouch for the very name it is meant to reject. It
/// passed a live mutation until this line was added.
fn src_code_identifiers(root: &Path) -> HashSet<String> {
    let mut idents = HashSet::new();
    for path in src_rs_files(root) {
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        for line in before_test_module(&content).lines() {
            let mut rest = code_on_line(line);
            while let Some(start) = rest.find(|c: char| c.is_ascii_alphabetic() || c == '_') {
                let tail = &rest[start..];
                let end = tail
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(tail.len());
                idents.insert(tail[..end].to_owned());
                rest = &tail[end..];
            }
        }
    }
    idents
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Backtick-quoted `Type::member` pairs in `text`.
///
/// Exactly two segments, the first starting uppercase. A module path
/// (`sync::relock`) and a longer path (`crate::git::GitVcs`) are both outside
/// scope — neither asserts that a type has a member. A trailing `()` is
/// accepted and dropped.
///
/// A rustdoc bracketed link yields the same pair as a plain mention, since
/// ``[`VcsError::NotARepo`]`` and `` `VcsError::NotARepo` `` hold the same
/// backtick span.
fn qualified_doc_refs(text: &str) -> Vec<(&str, &str)> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .filter_map(|span| {
            let (ty, member) = span.strip_suffix("()").unwrap_or(span).split_once("::")?;
            let uppercase_first = ty.starts_with(|c: char| c.is_ascii_uppercase());
            (uppercase_first && is_ident(ty) && is_ident(member)).then_some((ty, member))
        })
        .collect()
}

/// Enforce **definition-existence** for the symbols a `src/` comment names on
/// a type of this repository's own: if `Type` occurs in the code here, a
/// comment writing `` `Type::member` `` asserts that `member` exists, and it
/// must occur in code here too.
///
/// The predicate is occurrence-in-code, not occurrence. A symbol that was just
/// deleted is a symbol people write comments about, so counting every mention
/// decouples from the question being asked exactly when a stale reference is
/// most likely: a method deleted from the VCS seam kept eleven mentions across
/// seven files, all of them prose about its removal, and a mention count read
/// that tree as clean. Comment text is the surface making the claim and cannot
/// also be the evidence for it.
///
/// # This gate is narrow, and green does not mean the tree is clean
///
/// It covers the **qualified** shape only. A comment naming a deleted symbol
/// as a bare identifier — the shape that motivated this gate, a live trait
/// member listed among four surviving ones — is **not** caught, here or
/// anywhere.
///
/// That is a measured limit rather than an oversight. Applied to every
/// backtick-quoted bare identifier in `src/` comments, the same predicate
/// reports 28 sites, of which 21 are correct: names from `std` and from
/// dependencies, shell commands, hex digits in an example, names of test
/// files, parameters documented but not spelled in the signature, and one
/// ordinary English word. Suppressing those needs five mutually incompatible
/// justifications, and an inline annotation has to state a reason that is
/// literally true at the site it sits on; one annotation covering all five
/// says only that the gate is wrong here, which is an allowlist written
/// inline. Restricting the shape does not separate the populations either —
/// requiring two or more underscores drops the motivating case and still
/// leaves five wrong reports.
///
/// # The one shape it reports wrongly
///
/// The qualifier filter asks whether the *name* is used here, not whether the
/// type is declared here, so it drops a type this repository never touches
/// (`Decor::set_prefix` is silent) but keeps one it does. Documenting a `std`
/// or dependency method that this repository never calls, on a type it does
/// use, is therefore reported: `into_boxed_path` written against `PathBuf`
/// fails the gate even though it names a real method correctly. Both halves
/// are pinned by tests below, the wrong one included, because a limit nobody
/// wrote down is how the rule this gate replaces came to be believed.
///
/// No site in this repository hits it — 450 comment occurrences are in scope
/// and none is reported — and the way out is the one this paragraph just
/// took: drop the qualifier. An unqualified mention claims no membership here
/// and is out of scope by construction. That is why there is no escape hatch;
/// a hatch needs a reason that is literally true wherever it sits, and this
/// case has a plainer fix than annotating it.
fn check_doc_symbol_refs(
    root: &Path,
    files: &[PathBuf],
    code_idents: &HashSet<String>,
) -> Vec<String> {
    let mut errors = Vec::new();
    for path in files {
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        let rel = path.strip_prefix(root).unwrap_or(path).display();
        for (n, line) in before_test_module(&content).lines().enumerate() {
            let Some((_, text)) = comment_on_line(line) else {
                continue;
            };
            for (ty, member) in qualified_doc_refs(text) {
                if code_idents.contains(ty) && !code_idents.contains(member) {
                    errors.push(format!(
                        "{}:{}: `{ty}::{member}` — `{ty}` is a type here, but `{member}` \
                         appears in no code in this repository",
                        rel,
                        n + 1
                    ));
                }
            }
        }
    }
    errors
}

fn run_doc_symbol_check(root: &Path) -> Vec<String> {
    check_doc_symbol_refs(root, &src_rs_files(root), &src_code_identifiers(root))
}

/// Words that name a specific consumer or workflow rwv happens to be used
/// from, with no meaning in rwv's own domain model.
///
/// rwv ships standalone (Homebrew/PyPI); its source never references its
/// consumers — `check_no_tracker_ids` makes that structural for tracker IDs,
/// and this makes it structural for the words that name the tools around
/// those IDs. A cloner who has never touched any of these has no referent
/// for them.
///
/// The list is literal, not stemmed: it matches exactly `bead`/`choreograph`/
/// `subagent`/`sling`/`tl`/`epic` at word boundaries, not their plurals or
/// other inflections. That is deliberate — `beads-core` appears twice in
/// `src/` as an illustrative example crate name (manifest.rs,
/// integrations/cargo_workspace.rs) and matching the plural would force
/// rewording a hit that has nothing to do with the tracker. An entry names
/// known house vocabulary; a prior hit is not a precondition for adding one,
/// and this list is not a log of past violations. `workweave` is core rwv
/// vocabulary and must never be added here, and neither is `dispatch` — rwv's
/// own CLI dispatch uses the word throughout `src/`, so it cannot be added as
/// a bare word without banning rwv's own vocabulary alongside the consumer
/// sense of it.
const CONSUMER_VOCABULARY: &[&str] = &["bead", "choreograph", "subagent", "sling", "tl", "epic"];

/// True if `word` occurs in `haystack` (already lowercased) bounded on both
/// sides by a non-alphanumeric character or a string edge — i.e. as a whole
/// word, not as a substring of a longer identifier.
fn contains_word(haystack: &str, word: &str) -> bool {
    let b = haystack.as_bytes();
    haystack.match_indices(word).any(|(i, _)| {
        let before_ok = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        let after = i + word.len();
        let after_ok = after >= b.len() || !b[after].is_ascii_alphanumeric();
        before_ok && after_ok
    })
}

/// Scan `src/`, `docs/` and `tests/` for `CONSUMER_VOCABULARY`, at word
/// boundaries.
///
/// Same file scope as `check_no_tracker_ids` (via the shared
/// `src_docs_and_tests_files`) — this is the words-not-IDs sibling of that
/// gate and, like it, carries no `tests/` exemption: a consumer word is a
/// dead end for a standalone cloner in a test comment exactly as much as in
/// `src/`. One further exception: this file
/// (`src/bin/generate-explain.rs`) is skipped. It defines
/// `CONSUMER_VOCABULARY` and must spell each word out literally to check
/// for it, so scanning it here would have the gate flag its own
/// definition. `check_no_foreign_vocabulary` gets the same exception for
/// free — `FOREIGN_VOCABULARY` lives in this same file, just outside that
/// gate's docs-only scope.
fn check_no_consumer_vocabulary(root: &Path) -> Vec<String> {
    let self_path = Path::new("src/bin/generate-explain.rs");
    let files = src_docs_and_tests_files(root);
    if files.is_empty() {
        return vec![format!(
            "consumer-vocabulary check: no source/doc/test files found under {}",
            root.display()
        )];
    }
    let mut errors = Vec::new();
    for path in files {
        let rel = path.strip_prefix(root).unwrap_or(&path).to_owned();
        if rel == self_path {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        for (n, line) in content.lines().enumerate() {
            let lower = line.to_ascii_lowercase();
            for word in CONSUMER_VOCABULARY {
                if contains_word(&lower, word) {
                    errors.push(format!(
                        "{}:{}: consumer vocabulary `{word}`",
                        rel.display(),
                        n + 1
                    ));
                }
            }
        }
    }
    errors
}

/// Vocabulary belonging to a particular deployment of rwv rather than to rwv.
///
/// Every entry is matched via `contains_word`, bounded on ascii-alphanumeric
/// neighbours — not just `rig`, where `origin`, `right` and `trigger` all
/// contain the letters. The multi-word entries need the same bound: ordinary
/// prose can straddle a phrase the way "biogas cityscape" contains the raw
/// substring "gas city" without either word of the entry appearing on its
/// own. `contains_word` bounds only the two ends of the needle, so a needle
/// with internal spaces, parentheses or a dot — `"city (gc)"`, `"gc.city"` —
/// still gets a correct edge check.
const FOREIGN_VOCABULARY: &[&str] = &[
    "rig",
    "gas city",
    "city (gc)",
    "city(gc)",
    "gc agents",
    "gc session",
    "gc.city",
];

/// Scan `docs/` for vocabulary from a specific deployment of rwv.
///
/// `src/prime.rs` already asserts `rwv prime` stays free of it, but that test
/// only sees the prime overview. `rwv explain` prints
/// `docs/reference/explain/*.md` verbatim via `include_str!`, and one such
/// page shipped "the Gas City rig's standard …" to every user. Scanning the
/// whole of `docs/` covers the templates, the assembled pages, and the
/// published explanation joints in one place.
fn check_no_foreign_vocabulary(root: &Path) -> Vec<String> {
    let mut errors = Vec::new();
    for path in collect_md_files(&root.join("docs"), &[]) {
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let rel = path.strip_prefix(root).unwrap_or(&path).display();
        // Join wrapped lines: "the Gas\nCity rig's" escaped a line-oriented
        // grep once already.
        let flat = content.replace('\n', " ").to_ascii_lowercase();
        for term in FOREIGN_VOCABULARY {
            if contains_word(&flat, term) {
                errors.push(format!("{rel}: contains `{term}`"));
            }
        }
    }
    errors
}

/// Scan generated operator surfaces for `docs/internals/` paths.
///
/// The reader-axis rule is that operator-facing text references only pages
/// mdBook renders. `docs/internals/` is not in `docs/SUMMARY.md`, so a
/// reader who meets one of those paths on an operator surface has nothing to
/// open. The generator is an audience boundary — comment text that would be
/// legal in a clone becomes an operator surface once it is lifted onto one,
/// and the check makes that boundary executable.
///
/// Scope is the two directories the generator writes: assembled
/// `rwv explain` pages and their embedded schemas. Templates under
/// `docs/reference/explain/templates/` are excluded — the assembled output
/// is what a user reads, and it is what this gate reports on.
fn check_no_internals_on_operator_surfaces(root: &Path) -> Vec<String> {
    let mut errors = Vec::new();
    let explain_dir = root.join("docs/reference/explain");
    let templates_dir = explain_dir.join("templates");
    let schemas_dir = root.join("docs/reference/schemas");
    let mut files: Vec<PathBuf> = collect_md_files(&explain_dir, &[templates_dir])
        .into_iter()
        .collect();
    if schemas_dir.is_dir() {
        for entry in fs::read_dir(&schemas_dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.extension() == Some(OsStr::new("json")) {
                files.push(path);
            }
        }
    }
    for path in files {
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let rel = path.strip_prefix(root).unwrap_or(&path).display();
        for (n, line) in content.lines().enumerate() {
            if line.contains("docs/internals/") {
                errors.push(format!(
                    "{rel}:{}: contains `docs/internals/` — an operator surface \
                     references only pages `docs/SUMMARY.md` lists",
                    n + 1
                ));
            }
        }
    }
    errors
}

/// Check that `docs/SUMMARY.md` and `docs/explanation/index.md` list the same
/// set of joints pages.
///
/// `docs/SUMMARY.md` is the mdBook sidebar source of truth; it contains lines
/// of the form `- [Title](./explanation/joints/<stem>.md)`.
/// `docs/explanation/index.md` is the human-browsable landing page; it
/// contains lines of the form `- [Title](./joints/<stem>.md)`.
///
/// The two lists must agree exactly, because a reader browsing the explanation
/// landing page and a reader navigating the sidebar must see the same joints.
/// Neither file is generated, so agreement is convention-by-check rather than
/// convention-by-construction. This gate closes the class of omission:
/// adding a page to SUMMARY.md without a matching index entry (or vice versa)
/// will cause this check to fail rather than to silently lie to readers.
///
/// Returns one error message per discrepancy; empty means clean.
fn check_explanation_joints_index_agreement(root: &Path) -> anyhow::Result<Vec<String>> {
    // Parse docs/SUMMARY.md: collect every stem under ./explanation/joints/.
    let summary_path = root.join("docs/SUMMARY.md");
    let summary = fs::read_to_string(&summary_path).map_err(|e| {
        anyhow::anyhow!(
            "cannot read docs/SUMMARY.md at {}: {e}",
            summary_path.display()
        )
    })?;
    let summary_stems = collect_joints_stems_from_summary(&summary);

    // Parse docs/explanation/index.md: collect every stem under ./joints/.
    let index_path = root.join("docs/explanation/index.md");
    let index = fs::read_to_string(&index_path).map_err(|e| {
        anyhow::anyhow!(
            "cannot read docs/explanation/index.md at {}: {e}",
            index_path.display()
        )
    })?;
    let index_stems = collect_joints_stems_from_index(&index);

    let mut errors: Vec<String> = Vec::new();

    // Pages in SUMMARY.md but absent from index.md.
    for stem in &summary_stems {
        if !index_stems.contains(stem) {
            errors.push(format!(
                "explanation-joints: `{stem}.md` is listed in docs/SUMMARY.md \
                 under explanation/joints/ but is absent from docs/explanation/index.md — \
                 add `- [<Title>](./joints/{stem}.md)` to that file"
            ));
        }
    }

    // Pages in index.md but absent from SUMMARY.md.
    for stem in &index_stems {
        if !summary_stems.contains(stem) {
            errors.push(format!(
                "explanation-joints: `{stem}.md` is listed in docs/explanation/index.md \
                 but is absent from docs/SUMMARY.md under explanation/joints/ — \
                 add it to SUMMARY.md or remove it from index.md"
            ));
        }
    }

    Ok(errors)
}

/// Extract joint page stems from `docs/SUMMARY.md`.
///
/// Matches lines containing `./explanation/joints/<stem>.md` (the path form
/// SUMMARY.md uses for the explanation section).
fn collect_joints_stems_from_summary(content: &str) -> HashSet<String> {
    let mut stems = HashSet::new();
    for line in content.lines() {
        // Match the path fragment `./explanation/joints/<stem>.md` anywhere on the line.
        if let Some(start) = line.find("./explanation/joints/") {
            let after_prefix = &line[start + "./explanation/joints/".len()..];
            // The stem ends at the next `)` or end of token; strip the `.md)` suffix.
            if let Some(end) = after_prefix.find(')') {
                let filename = &after_prefix[..end];
                if let Some(stem) = filename.strip_suffix(".md") {
                    if !stem.is_empty() {
                        stems.insert(stem.to_owned());
                    }
                }
            }
        }
    }
    stems
}

/// Extract joint page stems from `docs/explanation/index.md`.
///
/// Matches lines containing `./joints/<stem>.md` (the relative path form the
/// index page uses — relative to `docs/explanation/`).
fn collect_joints_stems_from_index(content: &str) -> HashSet<String> {
    let mut stems = HashSet::new();
    for line in content.lines() {
        if let Some(start) = line.find("./joints/") {
            let after_prefix = &line[start + "./joints/".len()..];
            if let Some(end) = after_prefix.find(')') {
                let filename = &after_prefix[..end];
                if let Some(stem) = filename.strip_suffix(".md") {
                    if !stem.is_empty() {
                        stems.insert(stem.to_owned());
                    }
                }
            }
        }
    }
    stems
}

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at the crate root, which is the repoweave dir.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn write_if_changed(path: &Path, content: &str) -> std::io::Result<()> {
    // Avoid touching mtime when content is unchanged so incremental rebuilds
    // skip dependents that include_str! these files.
    if let Ok(existing) = fs::read_to_string(path) {
        if existing == content {
            return Ok(());
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)
}

fn main() -> anyhow::Result<()> {
    let root = repo_root();
    let templates_dir = root.join("docs/reference/explain/templates");
    let explain_dir = root.join("docs/reference/explain");
    let schemas_dir = root.join("docs/reference/schemas");

    // --- prime overview (separate template family, same render pattern) --------
    // Renders docs/reference/prime/templates/overview.md.tmpl →
    //         docs/reference/prime/overview.md
    // No {{SCHEMA}} substitution needed — the prime overview is static markdown.
    {
        let prime_tmpl_path = root.join("docs/reference/prime/templates/overview.md.tmpl");
        let prime_out_dir = root.join("docs/reference/prime");
        let prime_out_path = prime_out_dir.join("overview.md");

        let tmpl = fs::read_to_string(&prime_tmpl_path).map_err(|e| {
            anyhow::anyhow!(
                "missing prime overview template at {}: {e}",
                prime_tmpl_path.display()
            )
        })?;
        // No placeholder substitution — pure passthrough.
        let mut rendered = tmpl;
        if !rendered.ends_with('\n') {
            rendered.push('\n');
        }
        fs::create_dir_all(&prime_out_dir)?;
        write_if_changed(&prime_out_path, &rendered)?;
    }

    fs::create_dir_all(&explain_dir)?;
    fs::create_dir_all(&schemas_dir)?;

    let verbs = verbs();
    let msg_registry = build_msg_registry();

    for verb in &verbs {
        let template_path = templates_dir.join(format!("{}.md.tmpl", verb.name));
        let template = fs::read_to_string(&template_path).map_err(|e| {
            anyhow::anyhow!(
                "missing template for verb '{}' at {}: {e}",
                verb.name,
                template_path.display()
            )
        })?;

        let (schema_json_for_template, schema_artifact) = match verb.schema {
            Some(gen) => {
                let json = gen();
                // Flatten rustdoc intra-doc links that schemars pulls verbatim
                // from /// doc-comments into schema descriptions. The raw
                // artifact (written to docs/reference/schemas/) is left as-is
                // so tooling that consumes the JSON Schema directly gets the
                // unmodified output; only the human/agent-facing embedded copy
                // is cleaned.
                let clean = flatten_rustdoc_links(&json);
                (Some(clean), Some(json))
            }
            None => (None, None),
        };

        // Phase 1: splice {{SCHEMA}} with the schemars-derived JSON Schema block.
        let after_schema = render_template(&template, schema_json_for_template.as_deref());
        // Phase 2: splice {{MSG:<key>}} placeholders from the named-string registry.
        let rendered = resolve_msg_placeholders(&after_schema, &msg_registry)
            .map_err(|e| anyhow::anyhow!("verb '{}': {e}", verb.name))?;

        let md_path = explain_dir.join(format!("{}.md", verb.name));
        // Ensure trailing newline.
        let mut md = rendered;
        if !md.ends_with('\n') {
            md.push('\n');
        }
        write_if_changed(&md_path, &md)?;

        if let Some(schema) = schema_artifact {
            let mut content = schema;
            if !content.ends_with('\n') {
                content.push('\n');
            }
            let schema_path = schemas_dir.join(format!("{}.json", verb.name));
            write_if_changed(&schema_path, &content)?;
        }
    }

    // Sanity-check schema URLs the binary embeds match the artifact paths we
    // just wrote. If a URL drifts (e.g. someone changes the crate version
    // path), this surfaces immediately.
    assert!(
        SYNC_JSON_SCHEMA_URL.ends_with("/docs/reference/schemas/sync.json"),
        "SYNC_JSON_SCHEMA_URL no longer points at the committed artifact"
    );
    assert!(
        SYNC_TO_JSON_SCHEMA_URL.ends_with("/docs/reference/schemas/sync-to.json"),
        "SYNC_TO_JSON_SCHEMA_URL no longer points at the committed artifact"
    );
    assert!(
        repoweave::check::DOCTOR_SCHEMA_URL.ends_with("/docs/reference/schemas/doctor.json"),
        "DOCTOR_SCHEMA_URL no longer points at the committed artifact"
    );
    assert!(
        repoweave::status::STATUS_SCHEMA_URL.ends_with("/docs/reference/schemas/status.json"),
        "STATUS_SCHEMA_URL no longer points at the committed artifact"
    );
    assert!(
        repoweave::fetch::FETCH_SCHEMA_URL.ends_with("/docs/reference/schemas/fetch.json"),
        "FETCH_SCHEMA_URL no longer points at the committed artifact"
    );
    assert!(
        UPDATE_SCHEMA_URL.ends_with("/docs/reference/schemas/update.json"),
        "UPDATE_SCHEMA_URL no longer points at the committed artifact"
    );
    assert!(
        PUSH_SCHEMA_URL.ends_with("/docs/reference/schemas/push.json"),
        "PUSH_SCHEMA_URL no longer points at the committed artifact"
    );

    let index = render_index(&verbs);
    let index_path = explain_dir.join("index.md");
    write_if_changed(&index_path, &index)?;

    // --- CLI coverage gate -----------------------------------------------
    // Every subcommand in the rwv CLI tree must appear in docs/reference/cli.md
    // AND every top-level verb must be registered in verbs() above.
    // Deliberate omissions are recorded in docs/cli-coverage-allowlist.txt.
    // See the match-rule documentation on `run_coverage_checks`.
    let coverage_errors = run_coverage_checks(&root, &verbs)?;
    if !coverage_errors.is_empty() {
        let msg = coverage_errors.join("\n");
        anyhow::bail!(
            "CLI coverage check failed:\n{msg}\n\n\
             Fix: add a cli.md heading containing `rwv <path>` for each missing \
             subcommand, or add a Verb entry to verbs() for each missing registry \
             entry, or add the surface to docs/cli-coverage-allowlist.txt with a \
             reason if the omission is deliberate."
        );
    }

    // --- verb-registry gate -----------------------------------------------
    // verbs() above and repoweave::explain::known_verbs() (the runtime
    // dispatch table in src/explain.rs) must name the same set of verbs, or
    // `rwv explain <verb>` and the generated docs/schemas part ways silently.
    let runtime_verbs: Vec<&str> = explain::known_verbs().collect();
    let registry_errors = check_verb_registry_consistency(&verbs, &runtime_verbs);
    if !registry_errors.is_empty() {
        let msg = registry_errors.join("\n");
        anyhow::bail!(
            "verb registry mismatch:\n{msg}\n\n\
             Fix: add or remove the verb in both places so the two registries \
             name the same set."
        );
    }

    // --- schema-url gate ----------------------------------------------------
    // Every `<verb>_SCHEMA_URL` const is built from `crate::schema_url::
    // schema_url!` (src/schema_url.rs); no file may hand-spell the base URL.
    let schema_url_errors = run_schema_url_scheme_check(&root);
    if !schema_url_errors.is_empty() {
        let msg = schema_url_errors.join("\n");
        anyhow::bail!(
            "schema-URL scheme check failed:\n{msg}\n\n\
             Fix: build the const from `crate::schema_url::schema_url!(\"<verb>\")` \
             instead of spelling the URL out."
        );
    }

    // --- Link-cleanliness gate -------------------------------------------
    // Every relative markdown link in every .md file under docs/ must resolve
    // on disk; rustdoc intra-doc syntax must not appear in assembled output
    // (docs/reference/explain/ and docs/reference/prime/). Template
    // directories are excluded. This catches dead cross-doc links across the
    // entire doc tree, not only in assembled explain pages.
    let docs_dir = root.join("docs");
    let link_errors = check_assembled_docs(&docs_dir);
    if !link_errors.is_empty() {
        let msg = link_errors.join("\n");
        anyhow::bail!(
            "docs failed link-cleanliness check:\n{msg}\n\n\
             Fix: remove broken relative links from docs (use plain text for out-of-repo references) and ensure rustdoc \
             intra-doc link syntax is not present in assembled output."
        );
    }

    // --- env-input inventory gate ----------------------------------------
    // Every std::env::var / var_os read in non-test src/ code must be
    // recorded in docs/env-input-allowlist.txt with a reason. Any unlisted
    // read is a policy violation: argv addresses; env vars are handoff
    // surfaces set for child processes, never inputs consulted by rwv.
    // Deliberate reads are recorded in docs/env-input-allowlist.txt.
    let env_errors = run_env_input_check(&root)?;
    if !env_errors.is_empty() {
        let msg = env_errors.join("\n");
        anyhow::bail!(
            "env-input inventory check failed:\n{msg}\n\n\
             Fix: add `env-input:<VAR_NAME>` to docs/env-input-allowlist.txt \
             with a reason and a structural trigger for removal, or eliminate \
             the env read."
        );
    }

    // --- vcs-seam gate ----------------------------------------------------
    // `GitVcs` is private, so the compiler already refuses anyone who names
    // the backend. These are the two bypasses that name no type: minting one
    // through the `pub` constructor at a call site, and spawning git from
    // scratch. Both are production-only; a test module may do either.
    let seam_errors = check_vcs_seam_bypasses(&root.join("src"));
    if !seam_errors.is_empty() {
        let msg = seam_errors.join("\n");
        anyhow::bail!(
            "vcs-seam check failed:\n{msg}\n\n\
             Fix: accept a `&dyn Vcs` parameter from the frame that owns the \
             repo's identity, rather than resolving a backend where the work \
             happens."
        );
    }

    // --- envelope-output documentation gate ------------------------------
    // Every RWV_* variable set on plugin spawns by envelope_vars() in
    // src/plugins.rs must be documented in docs/reference/plugin-protocol.md.
    // The check calls envelope_vars() directly (no source grepping) so a new
    // variable added to the function is caught immediately. Failure tells the
    // author exactly which variable is undocumented and where to add it.
    let envelope_errors = run_envelope_output_check(&root)?;
    if !envelope_errors.is_empty() {
        let msg = envelope_errors.join("\n");
        anyhow::bail!(
            "envelope-output documentation check failed:\n{msg}\n\n\
             Fix: add a row for the variable to the Context envelope table in \
             docs/reference/plugin-protocol.md."
        );
    }

    // --- tracker-ID gate -------------------------------------------------
    // No tracker IDs in src/, docs/, or tests/. The rule decayed once
    // already after a manual scrub, so it is enforced here rather than
    // remembered.
    let tracker_errors = check_no_tracker_ids(&root);
    if !tracker_errors.is_empty() {
        let msg = tracker_errors.join("\n");
        anyhow::bail!(
            "tracker-ID check failed:\n{msg}\n\n\
             Fix: state the reason inline instead. A reader cannot open a \
             tracker ID, and these surfaces reach users through `rwv explain` \
             and error text. Commit messages are the place for the ID."
        );
    }

    // --- doc-citation gate -------------------------------------------------
    // A document a src/ comment cites must resolve from the repo root or from
    // the citing file's own directory, and a comment must not be nothing but
    // that reference. Enforces the path-resolution clause only — the
    // section-pointer clause of the same rule is caught only where the
    // document is named by a filename that does not resolve; see
    // check_doc_citations.
    let citation_errors = run_doc_citation_check(&root);
    if !citation_errors.is_empty() {
        let msg = citation_errors.join("\n");
        anyhow::bail!(
            "doc-citation check failed:\n{msg}\n\n\
             Fix: state the invariant in the comment. A citation a reader \
             cannot resolve from the repo root or from the citing file's own \
             directory is unfollowable from a clone — write the path that \
             resolves, or drop the reference and say the thing. A comment that \
             is only a pointer should be the sentence it was standing in for. \
             A path out of the repo that must stay takes \
             `weave-local-ref: <reason>` on the line above it."
        );
    }

    // --- doc-symbol gate ---------------------------------------------------
    // A comment writing `Type::member`, where Type is a type of this
    // repository's own, asserts that member exists — so member must appear in
    // code here, not merely in more comments. Covers the qualified shape only;
    // see check_doc_symbol_refs for what it does not catch.
    let symbol_errors = run_doc_symbol_check(&root);
    if !symbol_errors.is_empty() {
        let msg = symbol_errors.join("\n");
        anyhow::bail!(
            "doc-symbol check failed:\n{msg}\n\n\
             Fix: name a member that exists, or state the invariant without the \
             symbol. A name that appears in no code here is one that was deleted \
             or never existed, and prose about its removal is what the commit \
             message is for."
        );
    }

    // --- consumer-vocabulary gate ------------------------------------------
    // No consumer-specific words (the words-not-IDs sibling of the
    // tracker-ID gate above) in src/, docs/, or tests/. A standalone cloner
    // has no referent for them.
    let consumer_vocab_errors = check_no_consumer_vocabulary(&root);
    if !consumer_vocab_errors.is_empty() {
        let msg = consumer_vocab_errors.join("\n");
        anyhow::bail!(
            "consumer-vocabulary check failed:\n{msg}\n\n\
             Fix: reword in rwv's own terms — these words name a specific \
             consumer or workflow, and rwv ships standalone."
        );
    }

    // --- foreign-vocabulary gate -----------------------------------------
    // docs/ describes rwv, not any one deployment of it. `rwv explain` prints
    // these files verbatim, so a leak here reaches users.
    let vocab_errors = check_no_foreign_vocabulary(&root);
    if !vocab_errors.is_empty() {
        let msg = vocab_errors.join("\n");
        anyhow::bail!(
            "foreign-vocabulary check failed:\n{msg}\n\n\
             Fix: describe the behaviour in rwv's own terms."
        );
    }

    // --- operator-surface internals gate ---------------------------------
    // The generator is an audience boundary: comment text landing on
    // docs/reference/explain/** or docs/reference/schemas/*.json must not
    // point at docs/internals/, which mdBook does not render.
    let internals_errors = check_no_internals_on_operator_surfaces(&root);
    if !internals_errors.is_empty() {
        let msg = internals_errors.join("\n");
        anyhow::bail!(
            "operator-surface internals check failed:\n{msg}\n\n\
             Fix: state the rule inline, or point at a published page under \
             `docs/reference/` listed in `docs/SUMMARY.md`. `docs/internals/` \
             is not rendered."
        );
    }

    // --- explanation joints index-agreement gate -------------------------
    // docs/SUMMARY.md (sidebar source of truth) and docs/explanation/index.md
    // (human-browsable landing page) must list the same set of joints pages.
    // A page added to one without the other lies to readers who arrive through
    // the other path. Neither file is generated, so agreement is
    // convention-by-check.
    let joints_errors = check_explanation_joints_index_agreement(&root)?;
    if !joints_errors.is_empty() {
        let msg = joints_errors.join("\n");
        anyhow::bail!(
            "explanation joints index-agreement check failed:\n{msg}\n\n\
             Fix: add or remove the entry so docs/SUMMARY.md and \
             docs/explanation/index.md list the same joints pages."
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Build a minimal temp docs tree and return its root path.
    ///
    /// Layout:
    ///
    /// ```text
    /// <tmp>/docs/
    ///   how-to/guide.md         — hand-written, not assembled
    ///   reference/
    ///     explain/
    ///       templates/
    ///         verb.md.tmpl      — excluded (template)
    ///       verb.md             — assembled explain page
    ///     prime/
    ///       templates/
    ///         overview.md.tmpl  — excluded (template)
    ///       overview.md         — assembled prime page
    ///     target.md             — link target for cross-dir tests
    /// ```
    fn make_tree(tmp: &std::path::Path) -> PathBuf {
        let docs = tmp.join("docs");
        for d in &[
            "docs/how-to",
            "docs/reference/explain/templates",
            "docs/reference/prime/templates",
        ] {
            fs::create_dir_all(tmp.join(d)).unwrap();
        }
        // A "target" file that valid links can point to.
        fs::write(docs.join("reference/target.md"), "# target\n").unwrap();
        // Assembled explain page — valid link to ../target.md.
        fs::write(
            docs.join("reference/explain/verb.md"),
            "# verb\n\nSee [target](../target.md).\n",
        )
        .unwrap();
        // Assembled prime page — valid link to ../target.md.
        fs::write(
            docs.join("reference/prime/overview.md"),
            "# overview\n\nSee [target](../target.md).\n",
        )
        .unwrap();
        // Template files — must not be checked.
        fs::write(
            docs.join("reference/explain/templates/verb.md.tmpl"),
            "{{SCHEMA}}\n[broken](../nonexistent.md)\n",
        )
        .unwrap();
        fs::write(
            docs.join("reference/prime/templates/overview.md.tmpl"),
            "{{MSG:foo}}\n[broken](../nonexistent.md)\n",
        )
        .unwrap();
        // A hand-written how-to page with a valid relative link.
        fs::write(
            docs.join("how-to/guide.md"),
            "# guide\n\nSee [reference target](../reference/target.md).\n",
        )
        .unwrap();
        docs
    }

    /// The valid tree passes without errors.
    #[test]
    fn valid_tree_is_clean() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let docs = make_tree(tmp.path());
        let errors = check_assembled_docs(&docs);
        assert!(
            errors.is_empty(),
            "expected no errors in valid tree, got:\n{}",
            errors.join("\n")
        );
    }

    /// A broken relative link in a non-explain doc (how-to/) is reported.
    #[test]
    fn broken_link_in_non_explain_doc_is_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let docs = make_tree(tmp.path());
        // Overwrite the how-to guide with a broken link.
        fs::write(
            docs.join("how-to/guide.md"),
            "# guide\n\nSee [missing](../reference/nonexistent.md).\n",
        )
        .unwrap();
        let errors = check_assembled_docs(&docs);
        assert!(
            !errors.is_empty(),
            "expected at least one error for broken link, got none"
        );
        let combined = errors.join("\n");
        assert!(
            combined.contains("nonexistent.md"),
            "error should mention the broken target, got:\n{combined}"
        );
    }

    /// A broken relative link in an assembled explain doc is also reported.
    #[test]
    fn broken_link_in_explain_doc_is_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let docs = make_tree(tmp.path());
        // Overwrite the assembled explain page with a broken link.
        fs::write(
            docs.join("reference/explain/verb.md"),
            "# verb\n\nSee [gone](../does-not-exist.md).\n",
        )
        .unwrap();
        let errors = check_assembled_docs(&docs);
        assert!(
            !errors.is_empty(),
            "expected error for broken explain link, got none"
        );
    }

    /// Template files are excluded — broken links inside them must not be reported.
    #[test]
    fn template_files_are_excluded() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let docs = make_tree(tmp.path());
        // Templates already contain broken links in make_tree; the clean
        // base tree must pass.
        let errors = check_assembled_docs(&docs);
        assert!(
            errors.is_empty(),
            "template files leaked into check: {errors:?}"
        );
    }

    /// Rustdoc bare autolink syntax in an assembled explain doc is rejected.
    #[test]
    fn rustdoc_bare_autolink_in_assembled_doc_is_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let docs = make_tree(tmp.path());
        fs::write(
            docs.join("reference/explain/verb.md"),
            "# verb\n\n[`SomeType`] is interesting.\n",
        )
        .unwrap();
        let errors = check_assembled_docs(&docs);
        assert!(
            !errors.is_empty(),
            "expected error for bare autolink in assembled doc, got none"
        );
        let combined = errors.join("\n");
        assert!(
            combined.contains("rustdoc bare autolink"),
            "error should mention rustdoc autolink, got:\n{combined}"
        );
    }

    /// Rustdoc bare autolink in a non-assembled doc (how-to/) is NOT rejected
    /// by the link check — rustdoc leakage is only meaningful in assembled output.
    #[test]
    fn rustdoc_bare_autolink_in_non_assembled_doc_is_allowed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let docs = make_tree(tmp.path());
        // Hand-written docs may legitimately use backtick references like
        // [`Type`] as formatting; only assembled pages must be clean.
        fs::write(
            docs.join("how-to/guide.md"),
            "# guide\n\n[`SomeType`] is fine in hand-written docs.\n",
        )
        .unwrap();
        let errors = check_assembled_docs(&docs);
        assert!(
            errors.is_empty(),
            "should not flag bare autolink in non-assembled doc, got:\n{}",
            errors.join("\n")
        );
    }

    // ── Coverage check unit tests ──────────────────────────────────────────────

    /// A cli.md that covers all paths passes the cli-md check.
    #[test]
    fn cli_md_coverage_passes_when_all_present() {
        let paths = vec!["fetch".to_owned(), "workweave log".to_owned()];
        let cli_md =
            "### `rwv fetch [...]`\n\nSome text.\n\n### `rwv workweave log`\n\nMore text.\n";
        let allow: HashSet<String> = HashSet::new();
        let errors = check_cli_md_coverage(&paths, cli_md, &allow);
        assert!(
            errors.is_empty(),
            "expected no errors when all paths are covered, got:\n{}",
            errors.join("\n")
        );
    }

    /// Removing a cli.md heading causes the check to fail (seeded-failure proof).
    ///
    /// This test exercises the failure arm: a subcommand present in the CLI tree
    /// but absent from cli.md is reported as an error. Without this test, a
    /// check that never fires is indistinguishable from a correct check.
    #[test]
    fn cli_md_coverage_fails_when_entry_removed() {
        let paths = vec!["fetch".to_owned(), "workweave log".to_owned()];
        // cli.md only mentions `fetch` — `workweave log` heading was removed.
        let cli_md = "### `rwv fetch [...]`\n\nSome text.\n";
        let allow: HashSet<String> = HashSet::new();
        let errors = check_cli_md_coverage(&paths, cli_md, &allow);
        assert!(
            !errors.is_empty(),
            "expected error when `rwv workweave log` is absent from cli.md"
        );
        let combined = errors.join("\n");
        assert!(
            combined.contains("workweave log"),
            "error should name the missing subcommand, got:\n{combined}"
        );
        assert!(
            combined.contains("docs/reference/cli.md"),
            "error should name the file to fix, got:\n{combined}"
        );
    }

    /// An allowlisted path is not reported even when absent from cli.md.
    #[test]
    fn cli_md_coverage_skips_allowlisted_entry() {
        let paths = vec!["resolve".to_owned()];
        let cli_md = "# CLI reference\n\nNo resolve heading here.\n";
        let mut allow: HashSet<String> = HashSet::new();
        allow.insert("resolve".to_owned());
        let errors = check_cli_md_coverage(&paths, cli_md, &allow);
        assert!(
            errors.is_empty(),
            "allowlisted path should not be reported, got:\n{}",
            errors.join("\n")
        );
    }

    /// A `<placeholder>` positional interleaved between path components is
    /// skipped: `` `rwv workweave <project> log [--diff] [--json]` `` covers
    /// the path `workweave log` (clap allows the parent command's positionals
    /// before the nested subcommand: `rwv workweave [OPTIONS] [PROJECT]
    /// [COMMAND]`).
    #[test]
    fn cli_md_coverage_matches_interleaved_placeholder() {
        let paths = vec![
            "workweave log".to_owned(),
            "workweave create".to_owned(),
            "workweave list".to_owned(),
        ];
        let cli_md = "\
### `rwv workweave <project> create <name>`\n\n\
### `rwv workweave <project> list`\n\n\
### `rwv workweave <project> log [--diff] [--json]`\n";
        let allow: HashSet<String> = HashSet::new();
        let errors = check_cli_md_coverage(&paths, cli_md, &allow);
        assert!(
            errors.is_empty(),
            "placeholder-interleaved headings should cover the paths, got:\n{}",
            errors.join("\n")
        );
    }

    /// A literal token where a path component is expected fails the span
    /// match: `` `rwv workweave <project> list` `` must NOT cover
    /// `workweave log`, and a hyphen-extended token is not a prefix match.
    #[test]
    fn cli_md_coverage_placeholder_skip_does_not_overmatch() {
        let paths = vec!["workweave log".to_owned()];
        // Headings for OTHER workweave actions only — `log` is absent.
        let cli_md = concat!(
            "### `rwv workweave <project> create <name>`\n\n",
            "### `rwv workweave <project> list`\n\n",
            // rwv-advice: not-an-invocation
            "### `rwv workweave <project> log-extra`\n",
        );
        let allow: HashSet<String> = HashSet::new();
        let errors = check_cli_md_coverage(&paths, cli_md, &allow);
        assert!(
            !errors.is_empty(),
            "`workweave log` must not be covered by list/create/log-extra headings"
        );
    }

    /// Token-level rule spot checks on `span_covers_path` directly.
    #[test]
    fn span_covers_path_token_rules() {
        // Exact path, no extras.
        assert!(span_covers_path("rwv abort", &["abort"]));
        // Trailing args/flags after the last component are ignored.
        assert!(span_covers_path("rwv fetch <source> [...]", &["fetch"]));
        // Placeholder between components is skipped.
        assert!(span_covers_path(
            "rwv workweave <project> create <name>",
            &["workweave", "create"]
        ));
        // Span must start with `rwv`.
        assert!(!span_covers_path("cd $(rwv resolve)", &["resolve"]));
        // A different literal where a component is expected fails.
        assert!(!span_covers_path(
            "rwv workweave <project> delete <name>",
            &["workweave", "create"]
        ));
        // Components must all be present.
        assert!(!span_covers_path(
            "rwv workweave <project>",
            &["workweave", "log"]
        ));
    }

    /// All top-level verbs are registered — passes.
    #[test]
    fn registry_coverage_passes_when_all_registered() {
        let cli_verbs = vec!["fetch".to_owned(), "status".to_owned()];
        let verbs = vec![
            Verb {
                name: "fetch",
                summary: "clone or fetch",
                schema: None,
            },
            Verb {
                name: "status",
                summary: "show status",
                schema: None,
            },
        ];
        let allow: HashSet<String> = HashSet::new();
        let errors = check_registry_coverage(&cli_verbs, &verbs, &allow);
        assert!(
            errors.is_empty(),
            "expected no errors when all verbs are registered, got:\n{}",
            errors.join("\n")
        );
    }

    /// Removing a verb from verbs() causes the check to fail (seeded-failure proof).
    ///
    /// This test exercises the failure arm: a top-level CLI verb not present in
    /// the `verbs()` registry is reported as missing. Without this test, a check
    /// that never fires on the registry side is undetected.
    #[test]
    fn registry_coverage_fails_when_verb_unregistered() {
        let cli_verbs = vec!["fetch".to_owned(), "status".to_owned()];
        // Only `fetch` is registered; `status` was removed.
        let verbs = vec![Verb {
            name: "fetch",
            summary: "clone or fetch",
            schema: None,
        }];
        let allow: HashSet<String> = HashSet::new();
        let errors = check_registry_coverage(&cli_verbs, &verbs, &allow);
        assert!(
            !errors.is_empty(),
            "expected error when `status` is absent from verbs()"
        );
        let combined = errors.join("\n");
        assert!(
            combined.contains("status"),
            "error should name the missing verb, got:\n{combined}"
        );
        assert!(
            combined.contains("generate-explain.rs"),
            "error should name the file to fix, got:\n{combined}"
        );
    }

    /// An allowlisted verb is not reported even when absent from verbs().
    #[test]
    fn registry_coverage_skips_allowlisted_verb() {
        let cli_verbs = vec!["resolve".to_owned()];
        let verbs: Vec<Verb> = vec![];
        let mut allow: HashSet<String> = HashSet::new();
        allow.insert("resolve".to_owned());
        let errors = check_registry_coverage(&cli_verbs, &verbs, &allow);
        assert!(
            errors.is_empty(),
            "allowlisted verb should not be reported, got:\n{}",
            errors.join("\n")
        );
    }

    /// Matching registries — passes.
    #[test]
    fn verb_registry_consistency_passes_when_sets_match() {
        let verbs = vec![
            Verb {
                name: "fetch",
                summary: "clone or fetch",
                schema: None,
            },
            Verb {
                name: "status",
                summary: "show status",
                schema: None,
            },
        ];
        let runtime_verbs = vec!["fetch", "status"];
        let errors = check_verb_registry_consistency(&verbs, &runtime_verbs);
        assert!(
            errors.is_empty(),
            "expected no errors when both registries agree, got:\n{}",
            errors.join("\n")
        );
    }

    /// A verb present in `verbs()` but dropped from the runtime dispatch
    /// table (or vice versa) is reported by name (seeded-failure proof).
    #[test]
    fn verb_registry_consistency_fails_on_one_sided_verb() {
        let verbs = vec![
            Verb {
                name: "fetch",
                summary: "clone or fetch",
                schema: None,
            },
            Verb {
                name: "status",
                summary: "show status",
                schema: None,
            },
        ];
        // "status" is missing from the runtime side only.
        let runtime_verbs = vec!["fetch"];
        let errors = check_verb_registry_consistency(&verbs, &runtime_verbs);
        assert!(
            !errors.is_empty(),
            "expected an error when the two registries disagree"
        );
        let combined = errors.join("\n");
        assert!(
            combined.contains("status"),
            "error should name the disagreeing verb, got:\n{combined}"
        );
    }

    /// The base appearing only in `schema_url.rs` — passes.
    #[test]
    fn schema_url_scheme_passes_when_base_appears_only_in_schema_url_rs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("schema_url.rs");
        fs::write(
            &file,
            "macro_rules! schema_url { ($verb:literal) => { concat!(\"https://example.com/schemas/\", $verb, \".json\") }; }\n",
        )
        .unwrap();
        let errors = check_schema_url_scheme(tmp.path(), &[file], "https://example.com/schemas/");
        assert!(
            errors.is_empty(),
            "expected no errors when the base appears only in schema_url.rs, got:\n{}",
            errors.join("\n")
        );
    }

    /// A schema URL hand-spelled outside `schema_url.rs` is reported
    /// (seeded-failure proof).
    #[test]
    fn schema_url_scheme_fails_on_hand_spelled_base_elsewhere() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("fetch.rs");
        fs::write(
            &file,
            "pub const FETCH_SCHEMA_URL: &str = \"https://example.com/schemas/fetch.json\";\n",
        )
        .unwrap();
        let errors = check_schema_url_scheme(tmp.path(), &[file], "https://example.com/schemas/");
        assert!(
            !errors.is_empty(),
            "expected an error for a hand-spelled schema URL outside schema_url.rs"
        );
        let combined = errors.join("\n");
        assert!(
            combined.contains("fetch.rs"),
            "error should name the offending file, got:\n{combined}"
        );
    }

    /// Allowlist parsing accepts valid entries with inline reasons.
    #[test]
    fn allowlist_parsing_valid_entries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("allowlist.txt");
        fs::write(
            &path,
            "# comment\n\ncli-md:workweave log  # needs template\nregistry:resolve  # utility verb\n",
        )
        .unwrap();
        let (cli_md, registry) = load_coverage_allowlist(&path).expect("parse should succeed");
        assert!(cli_md.contains("workweave log"), "cli-md entry missing");
        assert!(registry.contains("resolve"), "registry entry missing");
    }

    /// Allowlist parsing rejects an entry missing an inline reason.
    #[test]
    fn allowlist_parsing_rejects_missing_reason() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("allowlist.txt");
        fs::write(&path, "registry:resolve\n").unwrap();
        let result = load_coverage_allowlist(&path);
        assert!(
            result.is_err(),
            "expected error for entry with no reason comment"
        );
    }

    /// Allowlist parsing rejects an unknown check type.
    #[test]
    fn allowlist_parsing_rejects_unknown_check_type() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("allowlist.txt");
        fs::write(&path, "unknown:foo  # reason\n").unwrap();
        let result = load_coverage_allowlist(&path);
        assert!(result.is_err(), "expected error for unknown check type");
    }

    // ── env-input inventory check unit tests ──────────────────────────────────

    /// env-input allowlist parsing accepts valid entries with inline reasons.
    #[test]
    fn env_input_allowlist_parsing_valid_entries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("env-input-allowlist.txt");
        fs::write(
            &path,
            "# comment\n\nenv-input:MY_VAR  # transitional; drop when the read is removed\n",
        )
        .unwrap();
        let vars = load_env_input_allowlist(&path).expect("parse should succeed");
        assert!(vars.contains("MY_VAR"), "MY_VAR entry missing");
    }

    /// env-input allowlist parsing rejects an entry with no reason comment.
    #[test]
    fn env_input_allowlist_parsing_rejects_missing_reason() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("env-input-allowlist.txt");
        fs::write(&path, "env-input:MY_VAR\n").unwrap();
        let result = load_env_input_allowlist(&path);
        assert!(
            result.is_err(),
            "expected error for entry with no reason comment"
        );
    }

    /// env-input allowlist parsing rejects an unknown check type.
    #[test]
    fn env_input_allowlist_parsing_rejects_unknown_check_type() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("env-input-allowlist.txt");
        fs::write(&path, "other:MY_VAR  # reason\n").unwrap();
        let result = load_env_input_allowlist(&path);
        assert!(result.is_err(), "expected error for unknown check type");
    }

    /// A src dir with no env reads passes the check.
    #[test]
    fn env_input_check_passes_with_no_reads() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "pub fn hello() {}\n").unwrap();
        let allow: HashSet<String> = HashSet::new();
        let errors = check_env_input_reads(&src, &allow);
        assert!(
            errors.is_empty(),
            "expected no errors for a file with no env reads, got:\n{}",
            errors.join("\n")
        );
    }

    /// An allowlisted env read does not produce an error.
    #[test]
    fn env_input_check_passes_with_allowlisted_read() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("lib.rs"),
            "pub fn f() { let _ = std::env::var(\"MY_VAR\"); }\n",
        )
        .unwrap();
        let mut allow: HashSet<String> = HashSet::new();
        allow.insert("MY_VAR".to_owned());
        let errors = check_env_input_reads(&src, &allow);
        assert!(
            errors.is_empty(),
            "allowlisted env read should not be reported, got:\n{}",
            errors.join("\n")
        );
    }

    /// An unlisted env read fails the check (seeded-failure proof).
    ///
    /// This test exercises the failure arm of the env-input inventory check:
    /// a std::env::var call not in the allowlist must be reported. Without this
    /// test, a check that never fires is indistinguishable from a correct check.
    #[test]
    fn env_input_check_fails_on_unlisted_read() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("lib.rs"),
            "pub fn f() { let _ = std::env::var(\"UNLISTED_VAR\"); }\n",
        )
        .unwrap();
        let allow: HashSet<String> = HashSet::new();
        let errors = check_env_input_reads(&src, &allow);
        assert!(
            !errors.is_empty(),
            "expected error for unlisted env read, got none"
        );
        let combined = errors.join("\n");
        assert!(
            combined.contains("UNLISTED_VAR"),
            "error should name the unlisted variable, got:\n{combined}"
        );
        assert!(
            combined.contains("env-input-allowlist.txt"),
            "error should name the allowlist file, got:\n{combined}"
        );
    }

    /// A production site that spawns git from scratch fails the seam check.
    ///
    /// The compiler closes the `GitVcs` name; nothing but this closes a raw
    /// spawn. A check that never fires is indistinguishable from a correct one.
    #[test]
    fn vcs_seam_check_fails_on_a_raw_git_spawn() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("verb.rs"),
            "pub fn f() { let _ = std::process::Command::new(\"git\").arg(\"status\"); }\n",
        )
        .unwrap();
        let errors = check_vcs_seam_bypasses(&src);
        assert_eq!(
            errors.len(),
            1,
            "expected one bypass, got:\n{}",
            errors.join("\n")
        );
        assert!(
            errors[0].contains("verb.rs:1") && errors[0].contains("spawns git directly"),
            "error should name the line and the bypass, got:\n{}",
            errors[0]
        );
    }

    /// A production site that mints its own backend fails the seam check.
    ///
    /// Minting dispatches correctly and can never be handed a double, so it
    /// is not a converted call site.
    #[test]
    fn vcs_seam_check_fails_on_a_minted_backend() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("verb.rs"),
            "pub fn f() { let vcs = crate::git::git_vcs(); let _ = vcs; }\n",
        )
        .unwrap();
        let errors = check_vcs_seam_bypasses(&src);
        assert_eq!(
            errors.len(),
            1,
            "expected one bypass, got:\n{}",
            errors.join("\n")
        );
        assert!(
            errors[0].contains("mints a git backend"),
            "error should name the bypass, got:\n{}",
            errors[0]
        );
    }

    /// Every shape rule 3 claims to see, planted in one file.
    ///
    /// Seeded in four places rather than one: a needle set this wide is
    /// exactly where one alternation quietly stops matching while the others
    /// carry the test, and a single plant could not tell that apart from a
    /// working check.
    #[test]
    fn vcs_seam_check_fails_on_each_on_disk_layout_shape() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("verb.rs"),
            "pub fn a(d: &Path) -> PathBuf { d.join(\".git\") }\n\
             pub fn b(d: &Path) -> PathBuf { d.join(\".git/worktrees\") }\n\
             pub fn c(d: &mut PathBuf) { d.push(\".git\"); }\n\
             pub fn e(n: &str) -> bool { n == \".git\" }\n",
        )
        .unwrap();
        let errors = check_vcs_seam_bypasses(&src);
        assert_eq!(
            errors.len(),
            4,
            "every seeded shape must be reported, got:\n{}",
            errors.join("\n")
        );
        for (i, line) in (1..=4).zip(&errors) {
            assert!(
                line.contains(&format!("verb.rs:{i}")) && line.contains("git's on-disk layout"),
                "error should name the line and the bypass, got:\n{line}"
            );
        }
    }

    /// Trimming `.git` off a clone URL is not a path construction.
    ///
    /// The four production sites that do this ask the filesystem nothing.
    /// A needle that reported them would be turned off, so this pins that it
    /// does not — and pins the working-tree files whose names merely start
    /// the same way.
    #[test]
    fn vcs_seam_check_ignores_url_suffixes_and_working_tree_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("verb.rs"),
            "pub fn a(u: &str) -> &str { u.strip_suffix(\".git\").unwrap_or(u) }\n\
             pub fn b(u: &str) -> &str { u.trim_end_matches(\".git\") }\n\
             pub fn c(d: &Path) -> PathBuf { d.join(\".gitignore\") }\n\
             pub fn e(d: &Path) -> PathBuf { d.join(\".gitmodules\") }\n\
             pub fn f(d: &Path) -> PathBuf { d.join(\".gitattributes\") }\n",
        )
        .unwrap();
        let errors = check_vcs_seam_bypasses(&src);
        assert!(
            errors.is_empty(),
            "URL trimming and working-tree files are not layout knowledge, got:\n{}",
            errors.join("\n")
        );
    }

    /// `src/vcs.rs` resolves backends; `src/git.rs` is the seam itself.
    #[test]
    fn vcs_seam_check_allows_the_resolver_and_the_backend_module() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("vcs.rs"),
            "pub fn project_vcs() -> Box<dyn Vcs> { crate::git::git_vcs() }\n",
        )
        .unwrap();
        fs::write(
            src.join("git.rs"),
            "pub fn git_vcs() -> Box<dyn Vcs> { Box::new(GitVcs) }\n\
             fn run() { let _ = std::process::Command::new(\"git\"); }\n",
        )
        .unwrap();
        let errors = check_vcs_seam_bypasses(&src);
        assert!(
            errors.is_empty(),
            "resolver and backend module are the seam, got:\n{}",
            errors.join("\n")
        );
    }

    /// A test module may build a concrete backend — that is what `git_vcs` is
    /// `pub` for. The boundary is any test module, not one named `tests`.
    #[test]
    fn vcs_seam_check_excludes_test_modules_under_any_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("verb.rs"),
            "pub fn f() {}\n\
             #[cfg(test)]\n\
             mod seam_tests {\n    \
             fn t() { let _ = crate::git::git_vcs(); \
             let _ = std::process::Command::new(\"git\"); }\n\
             }\n",
        )
        .unwrap();
        let errors = check_vcs_seam_bypasses(&src);
        assert!(
            errors.is_empty(),
            "test-module bypasses are out of scope, got:\n{}",
            errors.join("\n")
        );
    }

    /// env reads inside a `#[cfg(test)] mod tests` block are excluded.
    #[test]
    fn env_input_check_excludes_test_module_reads() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        // Production code has no env reads; test module has one.
        fs::write(
            src.join("lib.rs"),
            "pub fn f() {}\n\n\
             #[cfg(test)]\nmod tests {\n    \
             fn t() { let _ = std::env::var(\"TEST_ONLY_VAR\"); }\n}\n",
        )
        .unwrap();
        let allow: HashSet<String> = HashSet::new();
        let errors = check_env_input_reads(&src, &allow);
        assert!(
            errors.is_empty(),
            "env reads inside a test module should be excluded, got:\n{}",
            errors.join("\n")
        );
    }

    /// The two shapes that leaked test-module content into this gate, each
    /// carrying an ordinary-quoted env read that must stay silent.
    ///
    /// Both were live in this repository and neither failed, which is luck
    /// rather than scope: every fixture inside the leaked windows writes its
    /// env reads with escaped quotes, and the gate's needle is the literal
    /// unescaped form. These two write them the ordinary way.
    #[test]
    fn env_input_check_excludes_reads_in_the_windows_that_used_to_leak() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        // A prose mention of the attribute below the real declaration.
        fs::write(
            src.join("prose.rs"),
            "pub fn f() {}\n\
             #[cfg(test)]\n\
             mod tests {\n\
             \x20   fn t() { let _ = std::env::var(\"TEST_ONLY_VAR\"); }\n\
             \x20   /// Cuts at #[cfg(test)] mod tests, which is this line.\n\
             \x20   fn u() {}\n\
             }\n",
        )
        .unwrap();
        // A test module under a name other than `tests`.
        fs::write(
            src.join("named.rs"),
            "pub fn f() {}\n\
             #[cfg(test)]\n\
             mod derived_content_tests {\n\
             \x20   fn t() { let _ = std::env::var_os(\"OTHER_TEST_VAR\"); }\n\
             }\n",
        )
        .unwrap();
        let allow: HashSet<String> = HashSet::new();
        let errors = check_env_input_reads(&src, &allow);
        assert!(
            errors.is_empty(),
            "test-module env reads are out of scope under every module name, \
             below any prose mention, got:\n{}",
            errors.join("\n")
        );
    }

    /// env reads on comment lines are excluded.
    #[test]
    fn env_input_check_excludes_comment_lines() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        // The env read is on a comment line — must not fire.
        fs::write(
            src.join("lib.rs"),
            "pub fn f() {}\n// let _ = std::env::var(\"COMMENT_VAR\");\n",
        )
        .unwrap();
        let allow: HashSet<String> = HashSet::new();
        let errors = check_env_input_reads(&src, &allow);
        assert!(
            errors.is_empty(),
            "env reads on comment lines should be excluded, got:\n{}",
            errors.join("\n")
        );
    }

    /// `tl` and `epic` are in `CONSUMER_VOCABULARY` and the check reports
    /// them — a check that finds nothing is indistinguishable from one that
    /// never runs.
    #[test]
    fn consumer_vocabulary_check_fails_on_tl_and_epic() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("lib.rs"),
            "// The TL signed off on this.\n// Each epic decomposes further.\n",
        )
        .unwrap();
        let errors = check_no_consumer_vocabulary(tmp.path());
        assert!(
            errors
                .iter()
                .any(|e| e.contains("consumer vocabulary `tl`")),
            "expected a `tl` hit, got:\n{}",
            errors.join("\n")
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("consumer vocabulary `epic`")),
            "expected an `epic` hit, got:\n{}",
            errors.join("\n")
        );
    }

    /// Words that merely contain the letters `tl` adjacently must not fire —
    /// `contains_word` requires `tl` to stand on its own, bounded by
    /// non-alphanumeric characters or a string edge on both sides.
    #[test]
    fn consumer_vocabulary_check_ignores_tl_look_alikes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("lib.rs"),
            "// Negotiated over TLS, rendered as HTML, and told them to settle.\n",
        )
        .unwrap();
        let errors = check_no_consumer_vocabulary(tmp.path());
        assert!(
            errors.is_empty(),
            "TLS/HTML/settle contain the letters `tl` but not the word, got:\n{}",
            errors.join("\n")
        );
    }

    /// `tests/` carries no exemption from this gate — a hit planted there,
    /// with no `src/` present at all, must still be reported.
    #[test]
    fn consumer_vocabulary_check_reaches_tests_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tests_dir = tmp.path().join("tests");
        fs::create_dir_all(&tests_dir).unwrap();
        fs::write(
            tests_dir.join("some_test.rs"),
            "//! Pins the epic decision this bead exists to enforce.\n",
        )
        .unwrap();
        let errors = check_no_consumer_vocabulary(tmp.path());
        assert!(
            errors
                .iter()
                .any(|e| e.contains("tests/some_test.rs")
                    && e.contains("consumer vocabulary `epic`")),
            "expected an `epic` hit under tests/, got:\n{}",
            errors.join("\n")
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("tests/some_test.rs")
                    && e.contains("consumer vocabulary `bead`")),
            "expected a `bead` hit under tests/, got:\n{}",
            errors.join("\n")
        );
    }

    /// `rig` is in `FOREIGN_VOCABULARY` and no entry in that list has a live
    /// hit anywhere in this repository's own `docs/` — so a check that finds
    /// nothing is indistinguishable from one that never ran until something
    /// plants a hit and asserts on it.
    #[test]
    fn foreign_vocabulary_check_fails_on_seeded_rig_hit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let docs = tmp.path().join("docs");
        fs::create_dir_all(&docs).unwrap();
        fs::write(docs.join("deploy.md"), "Ship it through the rig.\n").unwrap();
        let errors = check_no_foreign_vocabulary(tmp.path());
        assert!(
            errors.iter().any(|e| e.contains("contains `rig`")),
            "expected a `rig` hit, got:\n{}",
            errors.join("\n")
        );
    }

    /// `origin`, `right` and `trigger` all contain the letters `rig` and must
    /// not fire — the doc comment on `FOREIGN_VOCABULARY` names this hazard
    /// for the `rig` entry specifically.
    #[test]
    fn foreign_vocabulary_check_ignores_rig_look_alikes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let docs = tmp.path().join("docs");
        fs::create_dir_all(&docs).unwrap();
        fs::write(
            docs.join("deploy.md"),
            "The origin repo, the right approach, and what triggers a rebuild.\n",
        )
        .unwrap();
        let errors = check_no_foreign_vocabulary(tmp.path());
        assert!(
            errors.is_empty(),
            "origin/right/trigger contain the letters `rig` but not the word, got:\n{}",
            errors.join("\n")
        );
    }

    /// Multi-word entries take a different path through the check than `rig`
    /// does and had no test at all before this.
    #[test]
    fn foreign_vocabulary_check_fails_on_seeded_multiword_hit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let docs = tmp.path().join("docs");
        fs::create_dir_all(&docs).unwrap();
        fs::write(docs.join("deploy.md"), "Deployed from the Gas City rig.\n").unwrap();
        let errors = check_no_foreign_vocabulary(tmp.path());
        assert!(
            errors.iter().any(|e| e.contains("contains `gas city`")),
            "expected a `gas city` hit, got:\n{}",
            errors.join("\n")
        );
    }

    /// Multi-word entries are bounded like `rig` — `"gas city"` must not fire
    /// on "biogas cityscape", which contains the raw substring but neither
    /// word of the entry on its own.
    #[test]
    fn foreign_vocabulary_check_multiword_entry_respects_word_boundary() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let docs = tmp.path().join("docs");
        fs::create_dir_all(&docs).unwrap();
        fs::write(
            docs.join("deploy.md"),
            "The biogas cityscape cooled overnight.\n",
        )
        .unwrap();
        let errors = check_no_foreign_vocabulary(tmp.path());
        assert!(
            errors.is_empty(),
            "\"biogas cityscape\" straddles \"gas city\" but contains neither word of it, \
             expected no hit, got:\n{}",
            errors.join("\n")
        );
    }

    /// A tracker ID in `src/` is reported — the seeded-failure pin this check
    /// had none of before.
    ///
    /// The planted ID is assembled from two literals rather than written as
    /// one: this file is itself scanned by `check_no_tracker_ids` (it carries
    /// no self-file exemption, unlike `check_no_consumer_vocabulary`), and a
    /// literal tracker ID here would fail the real gate on this repo.
    #[test]
    fn tracker_id_check_fails_on_fo_prefixed_id() {
        let planted_id = format!("{}-{}", "fo", "ab12cd.3");
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("lib.rs"),
            format!("// Regression per {planted_id}.\npub fn f() {{}}\n"),
        )
        .unwrap();
        let errors = check_no_tracker_ids(tmp.path());
        assert!(
            errors.iter().any(|e| e.contains(&planted_id)),
            "expected a tracker-ID hit, got:\n{}",
            errors.join("\n")
        );
    }

    /// `tests/` carries no exemption from this gate either — a tracker ID
    /// planted there, with no `src/` present at all, must still be reported.
    ///
    /// Same assembled-ID reason as `tracker_id_check_fails_on_fo_prefixed_id`.
    #[test]
    fn tracker_id_check_reaches_tests_dir() {
        let planted_id = format!("{}-{}", "fo", "ab12cd.3");
        let tmp = tempfile::tempdir().expect("tempdir");
        let tests_dir = tmp.path().join("tests");
        fs::create_dir_all(&tests_dir).unwrap();
        fs::write(
            tests_dir.join("some_test.rs"),
            format!("// Regression per {planted_id}.\n"),
        )
        .unwrap();
        let errors = check_no_tracker_ids(tmp.path());
        assert!(
            errors
                .iter()
                .any(|e| e.contains("tests/some_test.rs") && e.contains(&planted_id)),
            "expected a tracker-ID hit under tests/, got:\n{}",
            errors.join("\n")
        );
    }

    /// A root-level `.md` file — `CLAUDE.md` above all, the file that states
    /// the tracker-ID ban — carries no exemption from it either.
    ///
    /// Same assembled-ID reason as `tracker_id_check_fails_on_fo_prefixed_id`.
    #[test]
    fn tracker_id_check_reaches_root_level_md() {
        let planted_id = format!("{}-{}", "fo", "ab12cd.3");
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("CLAUDE.md"),
            format!("Banned everywhere, per {planted_id}.\n"),
        )
        .unwrap();
        let errors = check_no_tracker_ids(tmp.path());
        assert!(
            errors
                .iter()
                .any(|e| e.contains("CLAUDE.md") && e.contains(&planted_id)),
            "expected a tracker-ID hit in root-level CLAUDE.md, got:\n{}",
            errors.join("\n")
        );
    }

    /// A root-level `.rs` file — `build.rs`, which sits outside `src/` —
    /// carries no exemption either.
    ///
    /// Same assembled-ID reason as `tracker_id_check_fails_on_fo_prefixed_id`.
    #[test]
    fn tracker_id_check_reaches_root_level_rs() {
        let planted_id = format!("{}-{}", "fo", "ab12cd.3");
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("build.rs"),
            format!("// Regression per {planted_id}.\nfn main() {{}}\n"),
        )
        .unwrap();
        let errors = check_no_tracker_ids(tmp.path());
        assert!(
            errors
                .iter()
                .any(|e| e.contains("build.rs") && e.contains(&planted_id)),
            "expected a tracker-ID hit in root-level build.rs, got:\n{}",
            errors.join("\n")
        );
    }

    /// `docs/**.md` is in scope, so a tracker ID in published prose is
    /// reported like one in code.
    ///
    /// Same assembled-ID reason as `tracker_id_check_fails_on_fo_prefixed_id`.
    #[test]
    fn tracker_id_check_reaches_docs_md() {
        let planted_id = format!("{}-{}", "fo", "ab12cd.3");
        let tmp = tempfile::tempdir().expect("tempdir");
        let docs = tmp.path().join("docs");
        fs::create_dir_all(&docs).unwrap();
        fs::write(
            docs.join("guide.md"),
            format!("The refusal this describes was fixed per {planted_id}.\n"),
        )
        .unwrap();
        let errors = check_no_tracker_ids(tmp.path());
        assert!(
            errors
                .iter()
                .any(|e| e.contains("docs/guide.md") && e.contains(&planted_id)),
            "expected a tracker-ID hit under docs/, got:\n{}",
            errors.join("\n")
        );
    }

    /// The matcher holds `fo-` and nothing else, which is the boundary the
    /// tracker-ID clause states: rwv's own `rwv-`-prefixed vocabulary is not
    /// a tracker prefix here, an embedded `fo-` is not a word, and a slug
    /// outside four-to-eight lowercase-or-digit characters is not a slug.
    ///
    /// The planted control carries the whole weight of this test: asserting
    /// only that the negatives stay silent would pass equally if the gate
    /// stopped reporting anything at all. Widening the matcher to a second
    /// prefix without amending that clause reddens the count.
    #[test]
    fn tracker_id_check_holds_the_fo_prefix_and_nothing_else() {
        let control = format!("{}-{}", "fo", "ab12cd");
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("lib.rs"),
            format!(
                "// The rwv-ours driver is planted as merge.rwv-ours.driver, and\n\
                 // .rwv-active names the marker rwv-workweave reads.\n\
                 // An info-abc12 token embeds the prefix inside a word.\n\
                 // Bounds: fo-abc, fo-abcdefghij and fo-ABC12 are not IDs.\n\
                 // Only this one is: {control}\n\
                 pub fn f() {{}}\n"
            ),
        )
        .unwrap();
        let errors = check_no_tracker_ids(tmp.path());
        assert_eq!(
            errors.len(),
            1,
            "only the planted control may be reported, got:\n{}",
            errors.join("\n")
        );
        assert!(
            errors[0].contains(&control),
            "the single hit must be the planted control, got:\n{}",
            errors[0]
        );
    }

    /// The boundary drops test content and keeps production code.
    #[test]
    fn before_test_module_removes_test_content() {
        let content = "fn prod() {}\n\n#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n";
        let kept = before_test_module(content);
        assert!(
            kept.contains("fn prod()"),
            "production code must be preserved"
        );
        assert!(!kept.contains("fn t()"), "test content must be dropped");
    }

    /// The boundary is a no-op when there is no test module.
    #[test]
    fn before_test_module_no_op_without_test_module() {
        let content = "fn prod() {}\n";
        assert_eq!(
            before_test_module(content),
            content,
            "content without test module must be unchanged"
        );
    }

    /// A file with two test modules ends at the **first**, not the last.
    ///
    /// `src/git.rs` is that file — `branch_model_tests` then
    /// `derived_content_tests`. Taking the last marker leaves the whole of the
    /// first module inside every scanning gate.
    #[test]
    fn the_boundary_is_the_first_test_module_not_the_last() {
        let content = "fn prod() {}\n\
                       #[cfg(test)]\n\
                       mod branch_model_tests {\n\
                       \x20   fn first() {}\n\
                       }\n\
                       #[cfg(test)]\n\
                       mod derived_content_tests {\n\
                       \x20   fn second() {}\n\
                       }\n";
        assert_eq!(
            before_test_module(content),
            "fn prod() {}\n",
            "the first test module ends the region"
        );
    }

    /// A comment writing the attribute inline is prose, not a boundary.
    ///
    /// This file carries several such mentions, and the lowest of them sits
    /// hundreds of lines below the real declaration — so accepting the module
    /// on the same line as the attribute returns that whole span to the
    /// env-input gate's scope.
    #[test]
    fn a_prose_mention_of_the_attribute_is_not_a_boundary() {
        let content = "/// The sentinel is `#[cfg(test)] mod tests {` on one line.\n\
                       fn prod() {}\n\
                       #[cfg(test)]\n\
                       mod tests {\n\
                       \x20   fn t() {}\n\
                       }\n";
        let kept = before_test_module(content);
        assert!(
            kept.contains("fn prod()"),
            "a prose mention above the code must not cut it off, got:\n{kept}"
        );
        assert!(
            !kept.contains("fn t()"),
            "the real declaration below it is still the boundary, got:\n{kept}"
        );
    }

    /// `#[cfg(test)] mod <name>;` gates a whole other file and opens no region.
    ///
    /// `src/vcs.rs` declares `pub(crate) mod testing;` that way in its first
    /// twenty lines, so reading it as a boundary puts nearly the whole file
    /// outside every gate that scopes through here.
    #[test]
    fn a_test_only_file_module_declaration_is_not_a_boundary() {
        let content = "use std::io;\n\
                       #[cfg(test)]\n\
                       pub(crate) mod testing;\n\
                       fn prod() {}\n\
                       #[cfg(test)]\n\
                       mod tests {\n\
                       \x20   fn t() {}\n\
                       }\n";
        let kept = before_test_module(content);
        assert!(
            kept.contains("fn prod()"),
            "a file-module declaration must not end the region, got:\n{kept}"
        );
        assert!(
            !kept.contains("fn t()"),
            "the inline module below it is the boundary, got:\n{kept}"
        );
    }

    /// **Non-vacuity pin for the scope helper, against the real `src/` tree.**
    ///
    /// Every gate here seeds its failure into a temp fixture, and a fixture has
    /// no `#[cfg(test)]` at its head, so a fixture is always scanned whole. A
    /// seeded-failure test therefore proves a gate *can* report; it cannot see
    /// a scope helper handing that gate fifteen lines of a 3390-line file.
    /// `src/vcs.rs` was in exactly that state for five gates at once, under a
    /// suite where every one of them was green.
    ///
    /// The structural check below is spelled out rather than calling
    /// `opens_module_body`. Reusing that helper would assert the
    /// implementation against itself and pass for every implementation of it,
    /// including the one this test exists to reject.
    #[test]
    fn every_src_file_is_scoped_at_a_real_test_module_or_not_at_all() {
        let files = src_rs_files(&repo_root());
        assert!(
            !files.is_empty(),
            "no source files walked — this test would assert on nothing"
        );

        let mut truncated = 0usize;
        let mut wrong = Vec::new();
        for path in &files {
            let Ok(content) = fs::read_to_string(path) else {
                continue;
            };
            let kept = before_test_module(&content).len();
            if kept == content.len() {
                continue;
            }
            truncated += 1;

            let cut = &content[kept..];
            let opens_a_body = cut
                .strip_prefix("#[cfg(test)]")
                .and_then(|rest| rest.trim_start().split_once("mod "))
                .is_some_and(|(visibility, after_mod)| {
                    visibility.split_whitespace().all(|w| w.starts_with("pub"))
                        && after_mod
                            .trim_start()
                            .trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '_')
                            .trim_start()
                            .starts_with('{')
                });
            if !opens_a_body {
                wrong.push(format!(
                    "{}:{}: cut at `{}`",
                    path.display(),
                    content[..kept].lines().count() + 1,
                    cut.lines().take(2).collect::<Vec<_>>().join(" / ")
                ));
            }
        }

        assert!(
            truncated > 0,
            "no file was scoped at all — the helper stopped finding boundaries, \
             so the check below never ran"
        );
        assert!(
            wrong.is_empty(),
            "scoped somewhere that is not an inline test module:\n{}",
            wrong.join("\n")
        );
    }

    /// **Pinning test for the `tests/` exemption, which is a debt and not a
    /// principle.**
    ///
    /// A prohibition resting on a cleanup nobody has done decays quietly,
    /// because widening the scope by one line looks like tidying and
    /// narrowing it looks like nothing at all. This runs the gate end to end
    /// rather than inspecting the collector it happens to call, so rewiring
    /// either one fails here and moving the boundary stays a decision someone
    /// writes down.
    ///
    /// The same citation is planted under `src/` and under `tests/`, and the
    /// `src/` half is what makes the other half mean anything: a gate that
    /// reported nothing at all would satisfy a bare "nothing under `tests/`"
    /// on its own. It is the disagreement that is the evidence.
    #[test]
    fn the_citation_gate_reads_src_only_and_tests_is_an_exemption_with_a_cost() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("tests")).unwrap();
        let cites = "// The tier-0 invariants are recorded in clone-topology.md.\n\
                     pub fn f() {}\n";
        fs::write(root.join("src/lib.rs"), cites).unwrap();
        fs::write(root.join("tests/some_test.rs"), cites).unwrap();

        let findings = run_doc_citation_check(root);

        assert!(
            findings.iter().any(|f| f.starts_with("src/lib.rs:")),
            "the gate reported nothing under src/, so its silence under tests/ \
             says nothing:\n{}",
            findings.join("\n")
        );
        let leaked: Vec<_> = findings
            .iter()
            .filter(|f| f.starts_with("tests/"))
            .collect();
        assert!(
            leaked.is_empty(),
            "`tests/` is outside this gate until its citations are cleared; \
             widening the scope is a decision, not a tidy-up:\n{leaked:#?}"
        );

        assert!(
            !collect_rs_files(&repo_root().join("tests")).is_empty(),
            "`tests/` holds no .rs files — the exemption above exempts nothing"
        );
    }

    // ── envelope-output coverage check unit tests ─────────────────────────────

    /// A plugin-protocol page that documents all envelope vars passes the check.
    #[test]
    fn envelope_output_check_passes_when_all_documented() {
        let protocol_md = "| `RWV_VERSION` | rwv semver | never |\n\
                      | `RWV_WORKSPACE` | primary workspace root | no workspace resolved |\n\
                      | `RWV_WORKWEAVE` | workweave identity | not in a workweave |\n\
                      | `RWV_PROJECT` | resolved project name | no project resolved |\n";
        let errors = check_envelope_output_documented(protocol_md);
        assert!(
            errors.is_empty(),
            "expected no errors when all vars are documented, got:\n{}",
            errors.join("\n")
        );
    }

    /// A page missing one envelope var name produces an error naming that var.
    ///
    /// This is the seeded-failure proof: the check must fire when a var is absent.
    /// Without this test, a check that never fires is indistinguishable from a
    /// correct one.
    #[test]
    fn envelope_output_check_fails_when_var_undocumented() {
        // Only RWV_VERSION is documented — the workspace vars are absent.
        let protocol_md = "| `RWV_VERSION` | rwv semver | never |\n";
        let errors = check_envelope_output_documented(protocol_md);
        assert!(
            !errors.is_empty(),
            "expected errors for undocumented envelope vars, got none"
        );
        let combined = errors.join("\n");
        // RWV_WORKSPACE must be named in the error.
        assert!(
            combined.contains("RWV_WORKSPACE"),
            "error must name RWV_WORKSPACE, got:\n{combined}"
        );
        // The error must send the author to the page that owns the wire
        // contract, not to the CLI reference that only links to it.
        assert!(
            combined.contains("docs/reference/plugin-protocol.md"),
            "error must name the file to fix, got:\n{combined}"
        );
    }

    /// The check covers RWV_WORKWEAVE even though it is only set when a workweave
    /// is resolved. The function uses a fully-populated Resolution to exercise all
    /// branches of envelope_vars(), including the workweave conditional.
    #[test]
    fn envelope_output_check_covers_conditional_var() {
        // Everything except RWV_WORKWEAVE is documented.
        let protocol_md = "| `RWV_VERSION` | rwv semver | never |\n\
                      | `RWV_WORKSPACE` | primary workspace root | no workspace resolved |\n\
                      | `RWV_PROJECT` | resolved project name | no project resolved |\n";
        let errors = check_envelope_output_documented(protocol_md);
        assert!(
            !errors.is_empty(),
            "expected error for missing RWV_WORKWEAVE, got none"
        );
        let combined = errors.join("\n");
        assert!(
            combined.contains("RWV_WORKWEAVE"),
            "error must name RWV_WORKWEAVE as the missing var, got:\n{combined}"
        );
    }

    // ── doc-citation check unit tests ─────────────────────────────────────────

    /// Write `src/lib.rs` with `body` into a temp repo that has a real
    /// `docs/explanation/joints/clone-topology.md` to resolve against, and
    /// return the gate's findings.
    fn citation_errors(body: &str) -> Vec<String> {
        citation_errors_with(&[], body)
    }

    /// `citation_errors`, plus `extra` files written relative to the repo root
    /// so a fixture can supply whatever a citation is supposed to resolve to.
    fn citation_errors_with(extra: &[(&str, &str)], body: &str) -> Vec<String> {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let joints = root.join("docs/explanation/joints");
        fs::create_dir_all(&joints).unwrap();
        fs::write(joints.join("clone-topology.md"), "# clone topology\n").unwrap();
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        for (rel, content) in extra {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, content).unwrap();
        }
        let file = src.join("lib.rs");
        fs::write(&file, body).unwrap();
        let operated = src_code_doc_filenames(root);
        check_doc_citations(root, &[file], &operated)
    }

    /// A resolving path as a trailing pointer, after the comment has said the
    /// thing, is the shape CLAUDE.md explicitly permits — the ~20 citations to
    /// `docs/explanation/joints/` in `src/` all look like this.
    #[test]
    fn resolving_trailing_pointer_is_allowed() {
        let errors = citation_errors(
            "// Everything below the workweave root belongs to the cloned\n\
             // workweave. See docs/explanation/joints/clone-topology.md.\n\
             pub fn f() {}\n",
        );
        assert!(
            errors.is_empty(),
            "a resolving trailing pointer must not be reported, got:\n{}",
            errors.join("\n")
        );
    }

    /// A path that leaves the repository is unfollowable from a clone.
    #[test]
    fn non_resolving_path_is_reported() {
        let errors = citation_errors(
            "// A workweave checkout is a full clone, not a linked worktree;\n\
             // see ../../../../projects/foundations/docs/repoweave/design.md.\n\
             pub fn f() {}\n",
        );
        let combined = errors.join("\n");
        assert!(
            combined.contains("resolves neither"),
            "a path outside the repository must be reported, got:\n{combined}"
        );
    }

    /// **Pinning test for the bare-filename clause.** The document exists in
    /// the fixture repo, at `docs/explanation/joints/clone-topology.md`, and is
    /// cited by filename alone.
    ///
    /// This fails two ways if the clause regresses: if `doc_path_tokens` goes
    /// back to requiring a `/`, the token is never examined; and if resolution
    /// is ever relaxed to "a file by this name exists somewhere in the repo",
    /// the citation passes. Both are the permissiveness this test exists to
    /// forbid — the reader holds a comment, not an index.
    #[test]
    fn bare_filename_citation_is_reported_though_the_file_exists_in_the_repo() {
        let errors = citation_errors(
            "// A workweave checkout is a full clone, not a linked worktree.\n\
             // The tier-0 invariants are in clone-topology.md.\n\
             pub fn f() {}\n",
        );
        let combined = errors.join("\n");
        assert!(
            combined.contains("`clone-topology.md`") && combined.contains("resolves neither"),
            "a bare filename must be reported even though the file exists here, got:\n{combined}"
        );
    }

    /// The repo root is one of the two bases a citation may be written from.
    #[test]
    fn bare_filename_resolving_from_the_repo_root_is_allowed() {
        let errors = citation_errors_with(
            &[("ARCHITECTURE.md", "# architecture\n")],
            "// The seam layering is what keeps the VCS backend swappable.\n\
             // ARCHITECTURE.md carries the argument.\n\
             pub fn f() {}\n",
        );
        assert!(
            errors.is_empty(),
            "a filename resolving from the repo root must not be reported, got:\n{}",
            errors.join("\n")
        );
    }

    /// The citing file's own directory is the other base. `src/lib.rs` naming
    /// `notes.md` means `src/notes.md`, and a reader opens it without being
    /// told where to look.
    #[test]
    fn bare_filename_resolving_from_the_citing_files_directory_is_allowed() {
        let errors = citation_errors_with(
            &[("src/notes.md", "# notes\n")],
            "// Resolution order is deliberate and argued for next door.\n\
             // notes.md carries the argument.\n\
             pub fn f() {}\n",
        );
        assert!(
            errors.is_empty(),
            "a filename resolving beside the citing file must not be reported, got:\n{}",
            errors.join("\n")
        );
    }

    /// Resolution from the citing file's directory is lexical, and a `..` run
    /// that climbs above the repo root fails rather than resolving.
    ///
    /// The fixture puts a real file immediately outside the repo, which is the
    /// developer's own surroundings. Answering the citation from there would
    /// hand the "path into the workspace this repo is developed in" case a pass
    /// on the one machine where nobody needs the check.
    #[test]
    fn a_citation_climbing_above_the_repo_root_is_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("design.md"), "# design\n").unwrap();
        let root = tmp.path().join("repo");
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        let file = src.join("lib.rs");
        fs::write(
            &file,
            "// A workweave checkout is a full clone, not a linked worktree.\n\
             // The argument is in ../../design.md.\n\
             pub fn f() {}\n",
        )
        .unwrap();
        let operated = src_code_doc_filenames(&root);
        let errors = check_doc_citations(&root, &[file], &operated);
        let combined = errors.join("\n");
        assert!(
            combined.contains("resolves neither"),
            "a path resolving only outside the repo must be reported, got:\n{combined}"
        );
    }

    /// Prose naming a file type is not naming a file.
    #[test]
    fn a_bare_extension_is_not_a_citation() {
        let errors = citation_errors(
            "// Every page under the docs tree is a .md file, so the walker\n\
             // filters on that extension.\n\
             pub fn f() {}\n",
        );
        assert!(
            errors.is_empty(),
            "a bare extension must not be treated as a filename, got:\n{}",
            errors.join("\n")
        );
    }

    /// A filename this repository's own code operates on is an artifact the
    /// program handles, not a document the comment cites. This is the
    /// string-literal carve-out of the rule, reaching the comment that
    /// describes the same operation.
    #[test]
    fn a_filename_the_code_operates_on_is_not_a_citation() {
        let errors = citation_errors(
            "/// Writes the verb listing the book's nav bar renders from.\n\
             pub fn write_index(root: &std::path::Path) {\n\
             \x20   let _ = root.join(\"docs/reference/explain/index.md\");\n\
             }\n\
             // The nav bar is generated, so index.md is rewritten every run\n\
             // rather than hand-edited.\n\
             pub fn f() {}\n",
        );
        assert!(
            errors.is_empty(),
            "a filename the code operates on must not be reported, got:\n{}",
            errors.join("\n")
        );
    }

    /// **Pinning test for the `#[cfg(test)]` drop in `src_code_doc_filenames`.**
    ///
    /// A fixture may write any filename it likes. If fixture text counted as
    /// evidence that the program operates on a file, the gate would vouch for
    /// the very citation it is meant to reject — and this repository really
    /// does write `clone-topology.md` from a test fixture while citing it bare
    /// in production comments elsewhere.
    #[test]
    fn a_filename_written_only_by_a_test_fixture_does_not_exempt_a_citation() {
        let errors = citation_errors(
            "// A workweave checkout is a full clone, not a linked worktree.\n\
             // The tier-0 invariants are in clone-topology.md.\n\
             pub fn f() {}\n\
             #[cfg(test)]\n\
             mod tests {\n\
             \x20   #[test]\n\
             \x20   fn fixture() {\n\
             \x20       let _ = std::path::Path::new(\"clone-topology.md\");\n\
             \x20   }\n\
             }\n",
        );
        let combined = errors.join("\n");
        assert!(
            combined.contains("`clone-topology.md`"),
            "a fixture-only filename must not exempt a live citation, got:\n{combined}"
        );
    }

    /// **Pinning test for the inline-test-module scope.**
    ///
    /// A `#[cfg(test)]` comment asks its reader to follow a reference exactly
    /// as any other comment does, whatever the module is called. Excluding
    /// them is what let three citations of a document this repository does not
    /// contain sit green in `src/git.rs`, `src/sync.rs` and `src/vcs.rs`.
    ///
    /// The resolving half is asserted in the same module: a scope that
    /// reported everything below the attribute would pass the first half on
    /// its own.
    #[test]
    fn a_citation_inside_a_test_module_is_in_scope_under_any_module_name() {
        let errors = citation_errors(
            "pub fn f() {}\n\
             #[cfg(test)]\n\
             mod derived_content_tests {\n\
             \x20   // The driver is the no-op side-pick (regenerable-regions.md D2).\n\
             \x20   // Tier-0 lives in docs/explanation/joints/clone-topology.md.\n\
             \x20   fn t() {}\n\
             }\n",
        );
        let combined = errors.join("\n");
        assert!(
            combined.contains("`regenerable-regions.md`"),
            "a test module does not put a citation outside the rule, got:\n{combined}"
        );
        assert!(
            !combined.contains("clone-topology.md"),
            "a resolving citation in the same module must stay silent, got:\n{combined}"
        );
    }

    /// **Pinning test for the one exemption: this gate's own fixture corpus.**
    ///
    /// The fixtures below are comment text inside string literals, which
    /// `comment_blocks` reads as comments — so the gate reports its own
    /// seeded-failure tests unless its own file stops at the test boundary.
    /// The production half of the same file is asserted in scope: an exemption
    /// that swallowed the whole file would silence roughly half this gate's
    /// live citations along with the fixtures.
    #[test]
    fn this_gates_own_fixtures_are_exempt_and_its_production_comments_are_not() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let dir = root.join("src/bin");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(Path::new(file!()).file_name().expect("a file name"));
        fs::write(
            &file,
            "// A fixture writes regenerable-regions.md; the gate reports it on demand.\n\
             pub fn f() {}\n\
             #[cfg(test)]\n\
             mod tests {\n\
             \x20   // The driver is the no-op side-pick (regenerable-regions.md D2).\n\
             \x20   fn t() {}\n\
             }\n",
        )
        .unwrap();
        let errors = check_doc_citations(root, &[file], &HashSet::new());
        assert_eq!(
            errors.len(),
            1,
            "exactly the production citation, not the fixture, got:\n{}",
            errors.join("\n")
        );
        assert!(
            errors[0].contains("generate-explain.rs:1"),
            "the reported site is the production comment, got:\n{}",
            errors[0]
        );
    }

    /// A citation is reported above a test boundary as well as below it.
    #[test]
    fn a_citation_in_production_code_is_reported() {
        let above = citation_errors(
            "// The driver is the no-op side-pick (regenerable-regions.md D2).\n\
             pub fn f() {}\n",
        );
        assert!(
            above.iter().any(|e| e.contains("regenerable-regions.md")),
            "the same citation above the boundary must be reported, got:\n{}",
            above.join("\n")
        );
    }

    /// A markdown link gives the reader the resolving path alongside the
    /// filename, so the block is followable as it stands.
    #[test]
    fn a_filename_shown_as_a_resolving_path_in_the_same_block_is_allowed() {
        let errors = citation_errors(
            "// The bottom tier of the stability stack\n\
             // ([clone-topology.md](../../docs/explanation/joints/clone-topology.md))\n\
             // is what a manifest slot must satisfy.\n\
             pub fn f() {}\n",
        );
        assert!(
            errors.is_empty(),
            "a filename accompanied by its resolving path must not be reported, got:\n{}",
            errors.join("\n")
        );
    }

    /// Satisfaction is block-level, not file-level. A resolving path in some
    /// other comment is not something the reader of *this* comment holds — and
    /// file-level would have excused two live bare citations in this
    /// repository, both in a file that spells the full path once, far away.
    #[test]
    fn a_resolving_path_elsewhere_in_the_file_does_not_satisfy_a_bare_citation() {
        let errors = citation_errors(
            "// The stability stack is described in\n\
             // docs/explanation/joints/clone-topology.md.\n\
             pub fn stated_here() {}\n\
             \n\
             // Detached HEAD breaks the ref-namespace invariant recorded in\n\
             // clone-topology.md.\n\
             pub fn f() {}\n",
        );
        let combined = errors.join("\n");
        assert!(
            combined.contains("`clone-topology.md`") && combined.contains("resolves neither"),
            "a distant path must not satisfy a bare citation, got:\n{combined}"
        );
    }

    /// The bare-pointer clause: a comment whose entire content is the
    /// reference points instead of stating, and is a violation even though the
    /// path resolves.
    #[test]
    fn bare_pointer_is_reported_even_when_path_resolves() {
        let errors =
            citation_errors("// See docs/explanation/joints/clone-topology.md.\npub fn f() {}\n");
        let combined = errors.join("\n");
        assert!(
            combined.contains("points instead of stating"),
            "a comment that is only a reference must be reported, got:\n{combined}"
        );
        assert!(
            !combined.contains("resolves neither"),
            "the path resolves; only the bare-pointer clause should fire, got:\n{combined}"
        );
    }

    /// The inline hatch suppresses the resolution clause at one site. There is
    /// no allowlist file.
    #[test]
    fn local_ref_hatch_suppresses_the_resolution_clause() {
        let errors = citation_errors(
            "// The house comment policy this file follows is argued for in\n\
             // weave-local-ref: names the source of the policy, which is not vendored here; does not resolve in a standalone clone\n\
             // ../../../../projects/foundations/docs/agent-persona/philosophy.md.\n\
             pub fn f() {}\n",
        );
        assert!(
            errors.is_empty(),
            "an annotated out-of-repo path must not be reported, got:\n{}",
            errors.join("\n")
        );
    }

    /// A path inside a string literal is a program operating on a path, not a
    /// comment citing a document, and is out of scope — including the `//` of
    /// a URL, which must not be read as a comment start.
    #[test]
    fn string_literals_are_out_of_scope() {
        let errors = citation_errors(
            "const S: &str = \"https://example.com/docs/nowhere/missing.md\";\n\
             fn f() -> &'static str { \"docs/nowhere/missing.md\" }\n",
        );
        assert!(
            errors.is_empty(),
            "string literals must not be scanned, got:\n{}",
            errors.join("\n")
        );
    }

    /// A section pointer written against a bare filename is caught — not
    /// because it is a section pointer, but because the filename resolves from
    /// neither base. This is the shape the sweep before this gate was chasing,
    /// and the shape that used to pass unexamined.
    #[test]
    fn a_section_pointer_against_a_bare_filename_is_reported() {
        let errors = citation_errors(
            "// The refusal is the one the ownership-receipt arm describes\n\
             // (branch-model.md §3.3 arm 2).\n\
             pub fn f() {}\n",
        );
        let combined = errors.join("\n");
        assert!(
            combined.contains("`branch-model.md`"),
            "a section pointer against an unresolvable filename must be reported, got:\n{combined}"
        );
    }

    /// **The residual gap, pinned deliberately.** CLAUDE.md's section-pointer
    /// clause is still not mechanised *as such*: when the document is named by
    /// a path that resolves, the section pointer hanging off it is invisible
    /// here. The clause above narrowed this gap; it did not close it, and a
    /// green gate must not be read as "this tree has no section pointers".
    #[test]
    fn a_section_pointer_against_a_resolving_path_is_not_mechanised() {
        let errors = citation_errors(
            "// The refusal is the one the ownership-receipt arm describes\n\
             // (docs/explanation/joints/clone-topology.md §3.3 arm 2).\n\
             pub fn f() {}\n",
        );
        assert!(
            errors.is_empty(),
            "the section-pointer clause itself still has no matcher, got:\n{}",
            errors.join("\n")
        );
    }

    // ── operator-surface internals check unit tests ─────────────────────────

    /// A `docs/internals/` reference on an assembled explain page is caught.
    /// This is the seeded-failure test: the gate must report a fixture whose
    /// content it is meant to reject, not merely stay quiet on a clean tree.
    #[test]
    fn internals_path_on_assembled_explain_page_is_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let explain = root.join("docs/reference/explain");
        fs::create_dir_all(&explain).unwrap();
        fs::write(
            explain.join("doctor.md"),
            "# doctor\n\nSee docs/internals/branch-model.md.\n",
        )
        .unwrap();
        let errors = check_no_internals_on_operator_surfaces(root);
        let combined = errors.join("\n");
        assert!(
            combined.contains("docs/internals/") && combined.contains("doctor.md"),
            "an internals reference on an explain page must be reported, got:\n{combined}"
        );
    }

    /// A `docs/internals/` reference embedded in a generated schema is caught.
    #[test]
    fn internals_path_in_generated_schema_is_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let schemas = root.join("docs/reference/schemas");
        fs::create_dir_all(&schemas).unwrap();
        fs::write(
            schemas.join("doctor.json"),
            "{\"description\": \"See docs/internals/branch-model.md R2.\"}\n",
        )
        .unwrap();
        let errors = check_no_internals_on_operator_surfaces(root);
        let combined = errors.join("\n");
        assert!(
            combined.contains("docs/internals/") && combined.contains("doctor.json"),
            "an internals reference in a generated schema must be reported, got:\n{combined}"
        );
    }

    /// Templates under `docs/reference/explain/templates/` are the source that
    /// generation reads, not the operator surface generation writes. The gate
    /// scans only assembled output.
    #[test]
    fn internals_path_in_template_is_out_of_scope() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let templates = root.join("docs/reference/explain/templates");
        fs::create_dir_all(&templates).unwrap();
        fs::write(
            templates.join("doctor.md.tmpl"),
            "# doctor\n\nSee docs/internals/branch-model.md.\n",
        )
        .unwrap();
        let errors = check_no_internals_on_operator_surfaces(root);
        assert!(
            errors.is_empty(),
            "the gate must not report on templates, got:\n{}",
            errors.join("\n")
        );
    }

    /// A clean assembled tree passes.
    #[test]
    fn clean_operator_surfaces_are_allowed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let explain = root.join("docs/reference/explain");
        let schemas = root.join("docs/reference/schemas");
        fs::create_dir_all(&explain).unwrap();
        fs::create_dir_all(&schemas).unwrap();
        fs::write(
            explain.join("doctor.md"),
            "# doctor\n\nA ref that looks like rwv's is not rwv's.\n",
        )
        .unwrap();
        fs::write(
            schemas.join("doctor.json"),
            "{\"description\": \"Ownership is by record, never by name shape.\"}\n",
        )
        .unwrap();
        let errors = check_no_internals_on_operator_surfaces(root);
        assert!(
            errors.is_empty(),
            "a clean tree must pass, got:\n{}",
            errors.join("\n")
        );
    }

    // ── doc-symbol check unit tests ───────────────────────────────────────────

    /// Write `src/lib.rs` with `body` into a temp repo and return the gate's
    /// findings. The identifier set is built from the same file, so a fixture
    /// declares the code it wants to be believed about.
    fn symbol_errors(body: &str) -> Vec<String> {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        let file = src.join("lib.rs");
        fs::write(&file, body).unwrap();
        check_doc_symbol_refs(root, &[file], &src_code_identifiers(root))
    }

    /// The case the gate exists for: a member named on a repository type that
    /// survives only in prose. This is the deleted-VCS-method shape, written
    /// qualified.
    #[test]
    fn member_that_appears_only_in_comments_is_reported() {
        let errors = symbol_errors(
            "pub trait Vcs {\n    fn observe_head(&self);\n}\n\
             /// The `Vcs::current_ref` this replaced collapsed four states into one.\n\
             pub fn f() {}\n",
        );
        let combined = errors.join("\n");
        assert!(
            combined.contains("`Vcs::current_ref`"),
            "a member present only in prose must be reported, got:\n{combined}"
        );
    }

    /// Mention-count is the predicate this gate replaces: the deleted method
    /// keeps a crowd of comments discussing it, and every one of them must
    /// count as evidence for nothing.
    #[test]
    fn many_prose_mentions_do_not_rescue_a_deleted_member() {
        let errors = symbol_errors(
            "pub trait Vcs {\n    fn observe_head(&self);\n}\n\
             /// This restates the deleted `Vcs::current_ref` in terms of the four\n\
             /// states `Vcs::current_ref` answered with one `None`. The shipped\n\
             /// `Vcs::current_ref` read a detached HEAD as absent.\n\
             pub fn f() {}\n",
        );
        assert_eq!(
            errors.len(),
            3,
            "each prose mention is a finding, not evidence, got:\n{}",
            errors.join("\n")
        );
    }

    /// A member that exists in code is the ordinary case — 450 comment
    /// occurrences in this repository look like this and none may be reported.
    #[test]
    fn member_present_in_code_is_allowed() {
        let errors = symbol_errors(
            "pub trait Vcs {\n    fn observe_head(&self);\n}\n\
             /// Returns whatever `Vcs::observe_head` last reported.\n\
             pub fn f() {}\n",
        );
        assert!(
            errors.is_empty(),
            "a member that exists must not be reported, got:\n{}",
            errors.join("\n")
        );
    }

    /// A type this repository never touches is dropped before its member is
    /// looked up. Over-eagerness here is the regression that would get the
    /// gate reverted rather than fixed.
    #[test]
    fn members_of_unused_foreign_types_are_out_of_scope() {
        let errors = symbol_errors(
            "/// Prefix decoration (`Decor::set_prefix`) and `serde_json::Map`\n\
             /// are both foreign, as is `OpenOptions::create_new`.\n\
             pub fn f() {}\n",
        );
        assert!(
            errors.is_empty(),
            "members of types never used here must not be reported, got:\n{}",
            errors.join("\n")
        );
    }

    /// The gate's one wrong report, pinned deliberately. A `std` type this
    /// repository *does* use passes the qualifier filter, so a real method of
    /// it that the repository never calls is reported. Recording the limit as
    /// a test is the point: the rule this gate replaces was trusted for a case
    /// it could not cover because nobody had written the case down.
    #[test]
    fn foreign_method_on_a_used_type_is_reported_wrongly() {
        let errors = symbol_errors(
            "use std::path::PathBuf;\n\
             pub fn g(p: PathBuf) -> PathBuf { p }\n\
             /// Converts with `PathBuf::into_boxed_path`, which this crate never calls.\n\
             pub fn f() {}\n",
        );
        let combined = errors.join("\n");
        assert!(
            combined.contains("`PathBuf::into_boxed_path`"),
            "the known false report must stay visible, got:\n{combined}"
        );
    }

    /// The way out of the case above, and the reason it needs no escape hatch:
    /// an unqualified mention asserts no membership here, so it is out of
    /// scope by construction.
    #[test]
    fn dropping_the_qualifier_is_the_way_out() {
        let errors = symbol_errors(
            "use std::path::PathBuf;\n\
             pub fn g(p: PathBuf) -> PathBuf { p }\n\
             /// Converts with `into_boxed_path`, which this crate never calls.\n\
             pub fn f() {}\n",
        );
        assert!(
            errors.is_empty(),
            "an unqualified mention must not be reported, got:\n{}",
            errors.join("\n")
        );
    }

    /// A name registered as data rather than as an item still exists here. The
    /// identifier set keeps string literals for this reason.
    #[test]
    fn name_defined_only_in_a_string_literal_is_allowed() {
        let errors = symbol_errors(
            "pub struct Setup;\n\
             const EVENTS: &[&str] = &[\"WorktreeCreate\"];\n\
             /// Registers the command for `Setup::WorktreeCreate`.\n\
             pub fn f() {}\n",
        );
        assert!(
            errors.is_empty(),
            "a name present as data must not be reported, got:\n{}",
            errors.join("\n")
        );
    }

    /// The bare-identifier shape is the one that motivated this gate and the
    /// one it cannot cover; this pins the silence so a future sweep does not
    /// read green as "no stale symbol references". `current_ref` here is named
    /// exactly as the comment that prompted the work named it.
    #[test]
    fn bare_identifiers_are_not_mechanised() {
        let errors = symbol_errors(
            "pub trait Vcs {\n    fn observe_head(&self);\n}\n\
             /// The scanner consumes the `Vcs` trait — `current_ref`,\n\
             /// `observe_head` — without any git-specific code.\n\
             pub fn f() {}\n",
        );
        assert!(
            errors.is_empty(),
            "the bare-identifier shape has no matcher, got:\n{}",
            errors.join("\n")
        );
    }

    /// Test-module content is dropped, matching the doc-citation gate: a
    /// fixture naming a method it does not define is describing a scenario,
    /// not claiming an API.
    #[test]
    fn test_module_content_is_skipped() {
        let errors = symbol_errors(
            "pub struct Repo;\n\
             #[cfg(test)]\n\
             mod tests {\n    /// Pins that `Repo::vanished` stays gone.\n    fn t() {}\n}\n",
        );
        assert!(
            errors.is_empty(),
            "inline test content must not be scanned, got:\n{}",
            errors.join("\n")
        );
    }

    /// Only the two-segment shape asserts membership on a type. A module path
    /// and a longer path say something else, and a rustdoc bracketed link says
    /// the same thing as a plain mention.
    #[test]
    fn qualified_doc_refs_matches_only_type_member() {
        assert_eq!(
            qualified_doc_refs("holds `VcsError::NotARepo` and [`Vcs::observe_head`]"),
            vec![("VcsError", "NotARepo"), ("Vcs", "observe_head")]
        );
        assert_eq!(
            qualified_doc_refs("`Vcs::observe_head()`"),
            vec![("Vcs", "observe_head")]
        );
        assert!(
            qualified_doc_refs("`sync::relock` and `crate::git::GitVcs` and `plain`").is_empty(),
            "module paths, longer paths and bare names are not membership claims"
        );
    }

    // ── explanation joints index-agreement unit tests ─────────────────────────

    /// Both files list the same pages → no errors.
    #[test]
    fn joints_agreement_passes_when_sets_match() {
        let summary = "\
- [Pyramid of stability](./explanation/joints/pyramid-of-stability.md)\n\
- [Clone topology](./explanation/joints/clone-topology.md)\n";
        let index = "\
- [Pyramid of stability](./joints/pyramid-of-stability.md)\n\
- [Clone topology](./joints/clone-topology.md)\n";
        let summary_stems = collect_joints_stems_from_summary(summary);
        let index_stems = collect_joints_stems_from_index(index);
        assert_eq!(
            summary_stems, index_stems,
            "identical sets must agree: summary={summary_stems:?} index={index_stems:?}"
        );
    }

    /// A page in SUMMARY.md but missing from index.md is detected.
    #[test]
    fn joints_agreement_detects_summary_only_page() {
        let summary = "\
- [Pyramid of stability](./explanation/joints/pyramid-of-stability.md)\n\
- [Workweave lifecycle](./explanation/joints/workweave-lifecycle.md)\n";
        let index = "\
- [Pyramid of stability](./joints/pyramid-of-stability.md)\n";
        let summary_stems = collect_joints_stems_from_summary(summary);
        let index_stems = collect_joints_stems_from_index(index);
        assert!(
            summary_stems.contains("workweave-lifecycle"),
            "workweave-lifecycle must be in summary set"
        );
        assert!(
            !index_stems.contains("workweave-lifecycle"),
            "workweave-lifecycle must not be in index set"
        );
    }

    /// A page in index.md but missing from SUMMARY.md is detected.
    #[test]
    fn joints_agreement_detects_index_only_page() {
        let summary = "\
- [Pyramid of stability](./explanation/joints/pyramid-of-stability.md)\n";
        let index = "\
- [Pyramid of stability](./joints/pyramid-of-stability.md)\n\
- [Clone topology](./joints/clone-topology.md)\n";
        let summary_stems = collect_joints_stems_from_summary(summary);
        let index_stems = collect_joints_stems_from_index(index);
        assert!(
            index_stems.contains("clone-topology"),
            "clone-topology must be in index set"
        );
        assert!(
            !summary_stems.contains("clone-topology"),
            "clone-topology must not be in summary set"
        );
    }

    /// Non-joints lines in SUMMARY.md are not extracted.
    #[test]
    fn joints_stem_extractor_ignores_non_joints_lines() {
        let summary = "\
- [Introduction](./explanation/index.md)\n\
- [Workspace lens](./explanation/lenses/workspace.md)\n\
- [Pyramid of stability](./explanation/joints/pyramid-of-stability.md)\n";
        let stems = collect_joints_stems_from_summary(summary);
        assert_eq!(
            stems.len(),
            1,
            "only the joints line should be extracted, got: {stems:?}"
        );
        assert!(stems.contains("pyramid-of-stability"));
    }

    /// Non-joints lines in index.md (lenses section) are not extracted.
    #[test]
    fn index_stem_extractor_ignores_non_joints_lines() {
        let index = "\
- [Workspace lens](./lenses/workspace.md)\n\
- [Pyramid of stability](./joints/pyramid-of-stability.md)\n";
        let stems = collect_joints_stems_from_index(index);
        assert_eq!(
            stems.len(),
            1,
            "only the joints line should be extracted, got: {stems:?}"
        );
        assert!(stems.contains("pyramid-of-stability"));
    }

    /// Empty content produces an empty stem set (no panic, no spurious entries).
    #[test]
    fn joints_stem_extractors_handle_empty_content() {
        assert!(
            collect_joints_stems_from_summary("").is_empty(),
            "empty SUMMARY.md should yield no stems"
        );
        assert!(
            collect_joints_stems_from_index("").is_empty(),
            "empty index.md should yield no stems"
        );
    }
}
