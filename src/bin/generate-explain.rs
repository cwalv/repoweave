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
use serde::Serialize;

use repoweave::check::ViolationOutput;
use repoweave::cli::Cli;
use repoweave::fetch::FetchJsonOutput;
use repoweave::plugins::{envelope_vars, PluginRecord};
use repoweave::push::{PushJsonOutput, PUSH_SCHEMA_URL};
use repoweave::status::StatusJsonOutput;
use repoweave::sync::{
    auto_relock_commit_message, SyncJsonOutput, SyncToJsonOutput, SYNC_JSON_SCHEMA_URL,
    SYNC_TO_JSON_SCHEMA_URL,
};
use repoweave::update::{UpdateJsonOutput, UPDATE_SCHEMA_URL};
use repoweave::workspace::Resolution;

/// Output envelope for `rwv doctor --json`. By default only the active project
/// is checked and orphan detection is skipped; pass `--all` to scan every
/// project and enable weave-wide orphan detection. The `violations` array
/// contains one entry per finding; an empty array means the checked scope is
/// clean. The `plugins` array is the PATH inventory of `rwv-*` executables
/// (reporting only — plugin presence never fails the doctor check or affects
/// the exit code).
#[derive(Serialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct DoctorEnvelope {
    #[serde(rename = "$schema")]
    schema: String,
    violations: Vec<ViolationOutput>,
    /// `rwv-*` executables discovered on `PATH`. Each record carries the verb
    /// name, absolute path, and a `shadowed` flag for duplicates: when the
    /// same name appears in multiple `PATH` directories, the first copy wins
    /// at exec time; later copies are marked `shadowed: true` with
    /// `shadowed_by` pointing at the winning binary. Records are sorted by
    /// `(name, path)` for deterministic output. An empty array means no
    /// `rwv-*` executables were found. Never a failed check — the inventory
    /// is the audit surface for the PATH trust boundary.
    plugins: Vec<PluginRecord>,
    /// Resolved workspace coordinates (workspace root, optional workweave
    /// identity, project). Absent when no project is resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution: Option<Resolution>,
}

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
    let schema = schema_for!(DoctorEnvelope);
    serde_json::to_string_pretty(&schema).expect("doctor schema serializes")
}

/// Every `ViolationOutput` variant's `kind` tag, paired with its full variant
/// schema, walked from the schemars-derived doctor JSON Schema rather than a
/// hand-typed list — so it cannot drift out of sync with the enum.
fn doctor_violation_variants() -> Vec<(String, serde_json::Value)> {
    let schema = schema_for!(DoctorEnvelope);
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
    let schema = schema_for!(DoctorEnvelope);
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
/// - Example repo URLs (https://github.com/…)
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

/// Strip the `#[cfg(test)]` test module from Rust source content.
///
/// Finds the last occurrence of the two-line sequence `#[cfg(test)]\n`
/// followed by `mod tests` (with optional whitespace) and returns only the
/// content before that point. This cleanly excludes test-only env reads
/// (e.g. `std::env::set_var` / `remove_var` scaffolding in tests) without
/// requiring a full Rust parser.
///
/// If no test module marker is found the full content is returned unchanged.
fn strip_test_module(content: &str) -> &str {
    // Find `#[cfg(test)]` followed immediately by a newline and then `mod tests`.
    // We look for the pattern as a substring so we handle any indentation.
    let marker = "#[cfg(test)]";
    let mut search = content;
    let mut last_pos = None;
    while let Some(pos) = search.find(marker) {
        let abs_pos = content.len() - search.len() + pos;
        // After the marker, skip to the next line and check for `mod tests`.
        let after = &content[abs_pos + marker.len()..];
        let after_trimmed = after.trim_start_matches([' ', '\t', '\r', '\n']);
        if after_trimmed.starts_with("mod tests") {
            last_pos = Some(abs_pos);
        }
        // Advance past this occurrence.
        search = &search[pos + marker.len()..];
    }
    match last_pos {
        Some(p) => &content[..p],
        None => content,
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
/// would require a separate audit. Test modules (identified by the
/// `#[cfg(test)]\nmod tests` sentinel) are excluded before scanning.
/// Comment lines (leading `//`) are excluded to avoid false positives from
/// doc examples and inline annotations.
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

        // Exclude test module content (everything from #[cfg(test)] mod tests
        // to the end of the file).
        let production_content = strip_test_module(&content);

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
/// non-`.md` doc file these gates care about). Shared by `check_no_tracker_ids`
/// and `check_no_consumer_vocabulary` so the two gates never drift apart on
/// scope.
///
/// `tests/` is deliberately not included — its tracker IDs name the
/// regression each case pins, and its prose is free to describe the
/// consumer-specific scenario a test reproduces; both are a different
/// question from what these gates enforce on shipped `src/`+`docs/`.
fn src_and_docs_files(root: &Path) -> Vec<PathBuf> {
    let mut files = collect_rs_files(&root.join("src"));
    files.extend(collect_md_files(&root.join("docs"), &[]));
    files.push(root.join("docs/env-input-allowlist.txt"));
    files
}

/// Scan `src/` and `docs/` for tracker IDs in comments, error strings, and
/// published prose.
///
/// Code is ground truth and architecture docs carry rationale; a tracker ID
/// in either surface points a reader at something they cannot open, and it
/// reaches users directly through `rwv explain` (whose pages are `include_str!`'d
/// from `docs/reference/explain/`) and through `anyhow::bail!` text.
fn check_no_tracker_ids(root: &Path) -> Vec<String> {
    let mut errors = Vec::new();
    for path in src_and_docs_files(root) {
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

/// Path-shaped tokens in `text`: a run of path characters holding a `/`, whose
/// last component ends in a `DOC_PATH_EXTENSIONS` suffix.
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
            tok.contains('/')
                && tok
                    .rsplit('/')
                    .next()
                    .and_then(|last| last.rsplit_once('.'))
                    .is_some_and(|(_, ext)| DOC_PATH_EXTENSIONS.contains(&ext))
        })
        .collect()
}

/// True if `token` names a file present in a clone of this repository.
///
/// A leading `../` run is dropped first. A rustdoc link is written relative to
/// the generated HTML tree, not the repo root, and the question the rule asks
/// is whether a cloner holds the file — not whether the prefix would resolve
/// from `root`. Dropping the prefix cannot admit a genuine escape: the
/// remainder still has to name something that exists here.
fn resolves_in_repo(root: &Path, token: &str) -> bool {
    let mut rest = token;
    while let Some(next) = rest.strip_prefix("../").or_else(|| rest.strip_prefix("./")) {
        rest = next;
    }
    !rest.is_empty() && root.join(rest).exists()
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

/// Content up to the first `#[cfg(test)]` that is followed by a line break and
/// `mod tests`.
///
/// Not `strip_test_module`, which this gate cannot use. That one takes the
/// *last* marker and accepts `mod tests` on the same line, so a doc comment
/// mentioning `#[cfg(test)] mod tests` in prose counts as the boundary — this
/// file has several, and the last of them sits well below its own test module.
/// Requiring the line break admits only the real attribute; taking the first
/// admits only the real module.
fn before_test_module(content: &str) -> &str {
    let marker = "#[cfg(test)]";
    let mut from = 0;
    while let Some(pos) = content[from..].find(marker) {
        let at = from + pos;
        let after = content[at + marker.len()..].trim_start_matches([' ', '\t', '\r']);
        if after.starts_with('\n')
            && after
                .trim_start_matches([' ', '\t', '\r', '\n'])
                .starts_with("mod tests")
        {
            return &content[..at];
        }
        from = at + marker.len();
    }
    content
}

/// True if `line` is the inline escape-hatch annotation, with a reason.
fn is_local_ref_hatch(line: &str) -> bool {
    line.strip_prefix("weave-local-ref:")
        .is_some_and(|reason| !reason.trim().is_empty())
}

/// Enforce the **path-resolution clause** of `CLAUDE.md`'s "Comments do not
/// cite trackers or documents", over comments in `src/`:
///
/// - a path in a comment must name a file that exists in this repository. A
///   path into the workspace this repo is developed in is unfollowable from a
///   clone, and it rots invisibly, because nothing can check a path that was
///   never expected to resolve;
/// - a comment whose *entire* content is the reference is a violation even
///   when the path resolves — it points instead of stating. A resolving path
///   is legal as a trailing pointer, after the comment has said the thing.
///
/// A site may keep a path that leaves this repository by annotating the line
/// above it `weave-local-ref: <reason>`. The hatch suppresses the resolution
/// clause only; it is not a way to keep a comment that is nothing but a
/// pointer, and there is no allowlist file.
///
/// # This gate is not the whole rule
///
/// `CLAUDE.md` bans three shapes in comments. The tracker-ID shape is
/// `check_no_tracker_ids`. The third — a **section pointer** into a design
/// document (`branch-model.md §3.3`, `plan §7.1 arm 7`) — is **not mechanised
/// anywhere**, here or elsewhere. Around 290 bare `§` sites stand in `src/`
/// today, concentrated in `check.rs`, `sync.rs` and `vcs.rs`; a matcher for
/// them has to follow the sweep that removes them, not precede it. Reading a
/// green gate as "this tree has no section pointers" is wrong.
///
/// Three further limits, all taken deliberately against over-eagerness: only
/// the extensions in `DOC_PATH_EXTENSIONS` count as a document citation; a
/// path with no `/` is not a path token, so a bare filename in prose is not
/// checked; and inline test-module content is dropped (`before_test_module`),
/// as the env-input gate also drops it. That last one is a scope difference
/// from `check_no_tracker_ids` and `check_no_consumer_vocabulary`, which scan
/// whole files: those two ban text that reaches a user wherever it sits, while
/// this one asks whether a reader can follow a reference, and a test comment
/// describing the fixture tree it builds in a temp directory is not citing a
/// document.
fn check_doc_citations(root: &Path, files: &[PathBuf]) -> Vec<String> {
    let mut errors = Vec::new();
    for path in files {
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        let rel = path.strip_prefix(root).unwrap_or(path).display();
        for block in comment_blocks(before_test_module(&content)) {
            let joined = block.text();
            let block_tokens = doc_path_tokens(&joined);
            if block_tokens.is_empty() {
                continue;
            }
            for (i, (n, text)) in block.lines.iter().enumerate() {
                if i > 0 && is_local_ref_hatch(&block.lines[i - 1].1) {
                    continue;
                }
                for token in doc_path_tokens(text) {
                    if !resolves_in_repo(root, token) {
                        errors.push(format!(
                            "{rel}:{n}: `{token}` does not resolve to a file in this repository"
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
    check_doc_citations(root, &src_rs_files(root))
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
/// `subagent`/`sling` at word boundaries, not their plurals or other
/// inflections. That is deliberate — `beads-core` appears twice in `src/` as
/// an illustrative example crate name (manifest.rs, integrations/cargo_workspace.rs)
/// and matching the plural would force rewording a hit that has nothing to
/// do with the tracker. Add an entry only against a real hit in the tree;
/// `workweave` is core rwv vocabulary and must never be added here.
const CONSUMER_VOCABULARY: &[&str] = &["bead", "choreograph", "subagent", "sling"];

/// True if `word` occurs in `haystack` (already lowercased) bounded on both
/// sides by a non-alphanumeric character or a string edge — i.e. as a whole
/// word, not as a substring of a longer identifier. Same technique
/// `check_no_foreign_vocabulary` uses for `"rig"`.
fn contains_word(haystack: &str, word: &str) -> bool {
    let b = haystack.as_bytes();
    haystack.match_indices(word).any(|(i, _)| {
        let before_ok = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        let after = i + word.len();
        let after_ok = after >= b.len() || !b[after].is_ascii_alphanumeric();
        before_ok && after_ok
    })
}

/// Scan `src/` and `docs/` for `CONSUMER_VOCABULARY`, at word boundaries.
///
/// Same file scope and `tests/` exemption as `check_no_tracker_ids` (via
/// the shared `src_and_docs_files`) — this is the words-not-IDs sibling of
/// that gate, with one further exception: this file
/// (`src/bin/generate-explain.rs`) is skipped. It defines
/// `CONSUMER_VOCABULARY` and must spell each word out literally to check
/// for it, so scanning it here would have the gate flag its own
/// definition. `check_no_foreign_vocabulary` gets the same exception for
/// free — `FOREIGN_VOCABULARY` lives in this same file, just outside that
/// gate's docs-only scope.
fn check_no_consumer_vocabulary(root: &Path) -> Vec<String> {
    let self_path = Path::new("src/bin/generate-explain.rs");
    let mut errors = Vec::new();
    for path in src_and_docs_files(root) {
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
/// `rig` is matched at word boundaries because `origin`, `right` and `trigger`
/// all contain it.
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
            let hit = if *term == "rig" {
                let b = flat.as_bytes();
                flat.match_indices("rig").any(|(i, _)| {
                    let before_ok = i == 0 || !b[i - 1].is_ascii_alphanumeric();
                    let after = i + 3;
                    let after_ok = after >= b.len() || !b[after].is_ascii_alphanumeric();
                    before_ok && after_ok
                })
            } else {
                flat.contains(term)
            };
            if hit {
                errors.push(format!("{rel}: contains `{term}`"));
            }
        }
    }
    errors
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
    // No tracker IDs in src/ or docs/. The rule decayed once already after a
    // manual scrub, so it is enforced here rather than remembered.
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
    // A path in a src/ comment must name a file a cloner has, and a comment
    // must not be nothing but that reference. Enforces the path-resolution
    // clause only — the section-pointer clause of the same rule has no
    // matcher; see check_doc_citations.
    let citation_errors = run_doc_citation_check(&root);
    if !citation_errors.is_empty() {
        let msg = citation_errors.join("\n");
        anyhow::bail!(
            "doc-citation check failed:\n{msg}\n\n\
             Fix: state the invariant in the comment. A path that leaves this \
             repository is unfollowable from a clone; a comment that is only a \
             pointer should be the sentence it was standing in for. A path out \
             of the repo that must stay takes `weave-local-ref: <reason>` on \
             the line above it."
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
    // tracker-ID gate above) in src/ or docs/. A standalone cloner has no
    // referent for them.
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
            "env reads inside #[cfg(test)] mod tests should be excluded, got:\n{}",
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

    /// strip_test_module removes content from #[cfg(test)] mod tests onward.
    #[test]
    fn strip_test_module_removes_test_content() {
        let content = "fn prod() {}\n\n#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n";
        let stripped = strip_test_module(content);
        assert!(
            stripped.contains("fn prod()"),
            "production code must be preserved"
        );
        assert!(
            !stripped.contains("fn t()"),
            "test content must be stripped"
        );
    }

    /// strip_test_module is a no-op when there is no test module.
    #[test]
    fn strip_test_module_no_op_without_test_module() {
        let content = "fn prod() {}\n";
        let stripped = strip_test_module(content);
        assert_eq!(
            stripped, content,
            "content without test module must be unchanged"
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
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let joints = root.join("docs/explanation/joints");
        fs::create_dir_all(&joints).unwrap();
        fs::write(joints.join("clone-topology.md"), "# clone topology\n").unwrap();
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        let file = src.join("lib.rs");
        fs::write(&file, body).unwrap();
        check_doc_citations(root, &[file])
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
            combined.contains("does not resolve"),
            "a path outside the repository must be reported, got:\n{combined}"
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
            !combined.contains("does not resolve"),
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

    /// Section pointers are banned by CLAUDE.md but deliberately not mechanised
    /// here; this pins that the gate is silent about them, so a future sweep
    /// does not mistake a green gate for a clean tree.
    #[test]
    fn section_pointers_are_not_mechanised() {
        let errors = citation_errors(
            "// The refusal is the one branch-model.md §3.3 arm 2 describes.\n\
             pub fn f() {}\n",
        );
        assert!(
            errors.is_empty(),
            "the section-pointer clause has no matcher yet, got:\n{}",
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
}
