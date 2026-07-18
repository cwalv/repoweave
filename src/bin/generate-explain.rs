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
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use schemars::schema_for;
use serde::Serialize;

use repoweave::check::ViolationOutput;
use repoweave::fetch::FetchJsonOutput;
use repoweave::push::{PushJsonOutput, PUSH_SCHEMA_URL};
use repoweave::status::StatusJsonOutput;
use repoweave::sync::{
    auto_relock_commit_message, SyncJsonOutput, SyncToJsonOutput, SYNC_JSON_SCHEMA_URL,
    SYNC_TO_JSON_SCHEMA_URL,
};
use repoweave::update::{UpdateJsonOutput, UPDATE_SCHEMA_URL};

/// Output envelope for `rwv doctor --json`. By default only the active project
/// is checked and orphan detection is skipped; pass `--all` to scan every
/// project and enable weave-wide orphan detection. The `violations` array
/// contains one entry per finding; an empty array means the checked scope is
/// clean.
#[derive(Serialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct DoctorEnvelope {
    #[serde(rename = "$schema")]
    schema: String,
    violations: Vec<ViolationOutput>,
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

    // --- Link-cleanliness gate -------------------------------------------
    // Every relative markdown link in every .md file under docs/ must resolve
    // on disk; rustdoc intra-doc syntax must not appear in assembled output
    // (docs/reference/explain/ and docs/reference/prime/). Template
    // directories are excluded. This catches dead cross-doc links across the
    // entire doc tree, not only in assembled explain pages (fo-nto02q.4).
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
}
