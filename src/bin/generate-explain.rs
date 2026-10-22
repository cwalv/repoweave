//! `generate-explain` — build-time assembler for `rwv explain` artifacts, and
//! the runner for this repository's policy gates.
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
//! # Repo-policy gates
//!
//! Assembling artifacts is half of what `main` does. The other half is a set
//! of gates over the repository itself; a gate that reports anything fails
//! the run. They live in the generator rather than in a test because they
//! share the corpus walk and the scope definitions with generation, and
//! because running them here means regenerating the artifacts cannot skip
//! them. Someone looking for where a house rule about comments, vocabulary
//! or documentation coverage is mechanized looks in this file.
//!
//! Each is a `// --- <name> gate ---` block in `main`, in this order:
//!
//! - `run_coverage_checks` — every CLI subcommand appears in
//!   `docs/reference/cli.md`, and every top-level verb is registered in
//!   `verbs()`.
//! - `check_verb_registry_consistency` — `verbs()` and the runtime dispatch
//!   table name the same set.
//! - `run_schema_url_scheme_check` — no file hand-spells a schema base URL.
//! - `check_assembled_docs` — the link-cleanliness invariant below.
//! - `run_env_input_check` — every environment read in non-test `src/` code
//!   is listed in `docs/env-input-allowlist.txt`.
//! - `check_vcs_seam_bypasses` — production code takes the VCS seam from its
//!   caller instead of minting a backend or spawning git itself.
//! - `run_envelope_output_check` — every plugin envelope variable is
//!   documented in `docs/reference/plugin-protocol.md`.
//! - `check_no_tracker_ids` — no tracker ID in `src/`, `docs/` or `tests/`.
//! - `run_doc_citation_check` — a document a comment names resolves from the
//!   repo root or from the citing file's own directory, and no comment is
//!   only a pointer.
//! - `run_doc_symbol_check` — a comment writing a qualified member name
//!   names one that occurs in code.
//! - `check_no_consumer_vocabulary` — no word naming a consumer or workflow
//!   rwv happens to be used from.
//! - `check_no_foreign_vocabulary` — `docs/` describes rwv, not one
//!   deployment of it.
//! - `check_no_internals_on_operator_surfaces` — nothing on an assembled
//!   page, a generated schema, or a page `docs/SUMMARY.md` lists points at
//!   `docs/internals/`, which mdBook does not render.
//! - `check_vocabulary_and_spelling` — operator surfaces and message text
//!   use the canonical vocabulary, and all prose uses American spelling.
//! - `check_doc_tokens` — every finding kind, sub-kind, refusal token and
//!   `rwv` command an operator page cites exists in the code.
//! - `check_explanation_joints_index_agreement` — `docs/SUMMARY.md` and
//!   `docs/explanation/index.md` list the same joints pages.
//!
//! What a gate does not cover is stated on the predicate that implements it,
//! where a reader widening it will meet it. Green here means the corpus a
//! gate examined was clean, not that its rule holds over the tree.
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
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use clap::CommandFactory;
use regex::Regex;
use schemars::schema_for;

use repoweave::check::DoctorJsonOutput;
use repoweave::cli::Cli;
use repoweave::explain;
use repoweave::fetch::{FetchJsonOutput, FetchOutcomeNdjsonRecord, FETCH_RECORD_SCHEMA_URL};
use repoweave::placement::{PlacementNdjsonRecord, PLACEMENT_RECORD_SCHEMA_URL};
use repoweave::plugins::envelope_vars;
use repoweave::push::{
    PushJsonOutput, PushOutcomeNdjsonRecord, PUSH_RECORD_SCHEMA_URL, PUSH_SCHEMA_URL,
};
use repoweave::status::StatusJsonOutput;
use repoweave::sync::{
    auto_relock_commit_message, AdvanceNdjsonRecord, MemberNdjsonRecord, SyncJsonOutput,
    SyncOutcomeNdjsonRecord, SyncToJsonOutput, ADVANCE_RECORD_SCHEMA_URL, MEMBER_RECORD_SCHEMA_URL,
    SYNC_JSON_SCHEMA_URL, SYNC_RECORD_SCHEMA_URL, SYNC_TO_JSON_SCHEMA_URL,
    SYNC_TO_RECORD_SCHEMA_URL,
};
use repoweave::update::{
    UpdateJsonOutput, UpdateNdjsonRecord, UPDATE_RECORD_SCHEMA_URL, UPDATE_SCHEMA_URL,
};
use repoweave::workspace::Resolution;

// The bundled plugin's wire types and schema URLs, compiled into this binary
// so their schemas are generated and their URLs checked like a core verb's.
// Only the types are used here, never constructed.
#[path = "rwv-each/schema_url.rs"]
mod each_schema_url;
#[allow(dead_code)]
#[path = "rwv-each/wire.rs"]
mod each_wire;

/// The bundled plugins' source trees, which must never name the library.
const PLUGIN_SOURCE_DIRS: &[&str] = &["src/bin/rwv-each"];

/// True if `verb` is a bundled plugin's, which every release channel installs
/// beside `rwv`.
fn is_bundled_plugin(verb: &str) -> bool {
    PLUGIN_SOURCE_DIRS.iter().any(|dir| {
        Path::new(dir)
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("rwv-"))
            == Some(verb)
    })
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
    /// `Some` for verbs that also emit NDJSON records (`-j N`, `N > 1`); the
    /// closure returns the JSON Schema for the per-line record shape as a
    /// pretty-printed string. Written to `<verb>-record.json` alongside the
    /// envelope artifact. `None` when the verb has no NDJSON mode.
    schema_record: Option<fn() -> String>,
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

/// One row per registry per declared creation parameter, walked from
/// `builtin_registries()` — the same slice `rwv add --new`'s
/// missing-parameter refusal reads — rather than hand-typed, so a registry
/// that grows a parameter cannot leave this table behind.
fn creation_params_table_md() -> String {
    let registries = repoweave::registry::builtin_registries();
    let mut out = String::new();
    out.push_str("| Registry | Parameter | Required | Help |\n");
    out.push_str("|---|---|---|---|\n");
    for reg in &registries {
        for param in reg.creation_params() {
            out.push_str(&format!(
                "| `{}` | `{}` | {} | {} |\n",
                reg.name().as_str(),
                param.name,
                if param.required { "yes" } else { "no" },
                param.help
            ));
        }
    }
    out.truncate(out.trim_end().len());
    out
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

fn schema_fetch_record() -> String {
    let schema = schema_for!(FetchOutcomeNdjsonRecord<'static>);
    serde_json::to_string_pretty(&schema).expect("fetch-record schema serializes")
}

fn schema_placement_record() -> String {
    let schema = schema_for!(PlacementNdjsonRecord<'static>);
    serde_json::to_string_pretty(&schema).expect("placement-record schema serializes")
}

fn schema_member_record() -> String {
    let schema = schema_for!(MemberNdjsonRecord<'static>);
    let schema = inject_schema_field_into_oneof_branches(schema);
    serde_json::to_string_pretty(&schema).expect("member-record schema serializes")
}

fn schema_advance_record() -> String {
    let schema = schema_for!(AdvanceNdjsonRecord<'static>);
    let schema = inject_schema_field_into_oneof_branches(schema);
    serde_json::to_string_pretty(&schema).expect("advance-record schema serializes")
}

fn schema_update_record() -> String {
    let schema = schema_for!(UpdateNdjsonRecord<'static>);
    serde_json::to_string_pretty(&schema).expect("update-record schema serializes")
}

fn schema_push_record() -> String {
    let schema = schema_for!(PushOutcomeNdjsonRecord<'static>);
    let schema = inject_schema_field_into_oneof_branches(schema);
    serde_json::to_string_pretty(&schema).expect("push-record schema serializes")
}

fn schema_sync_record() -> String {
    let schema = schema_for!(SyncOutcomeNdjsonRecord<'static>);
    let schema = inject_schema_field_into_oneof_branches(schema);
    serde_json::to_string_pretty(&schema).expect("sync-record schema serializes")
}

/// Post-process a schemars-derived record schema so that a validator that
/// closes objects whenever `properties` is declared can validate the record
/// wire format.
///
/// # The generation problem
///
/// When a struct has a plain `#[serde(rename = "$schema")] schema: &str` field
/// alongside a `#[serde(flatten)]` enum field, schemars places:
///   - the outer struct's `$schema` field in top-level `required` + `properties`
///   - the flattened enum's variants in a top-level `oneOf`
///
/// This creates two conflicts for a validator that closes objects at any level
/// where `properties` is declared:
///
/// 1. **Top-level**: `properties: {"$schema": ...}` is present, so the
///    validator treats the top-level object as closed. The document's variant
///    fields (`kind`, `path`, `absolute_path`, …) appear undeclared there.
///
/// 2. **Branch-level**: each `oneOf` branch declares `properties` for its
///    variant fields, but not for `$schema`. The document's `$schema` field
///    appears undeclared inside each branch.
///
/// # The fix
///
/// For schemas that carry a top-level `oneOf` (i.e. the flattened field is an
/// enum) alongside top-level `required`/`properties` for `$schema`:
///
///   - Remove `$schema` from the top-level `properties` (keeping it in
///     `required` so the presence constraint is still enforced by the
///     `required` keyword rather than by property-level validation).
///   - Add `$schema: {"type":"string"}` to the `properties` of every `oneOf`
///     branch that already declares `properties`.
///
/// After this transformation the validator sees one open top-level object
/// (no `properties` key → no closed-object enforcement) with a `required`
/// constraint, and closed branches that each declare exactly the fields they
/// contain.
fn inject_schema_field_into_oneof_branches(
    schema: schemars::schema::RootSchema,
) -> schemars::schema::RootSchema {
    let mut raw = serde_json::to_value(&schema).expect("schema serializes for post-processing");
    let has_oneof = raw
        .get("oneOf")
        .and_then(|v| v.as_array())
        .is_some_and(|b| !b.is_empty());
    if !has_oneof {
        return schema;
    }

    // 1. Remove "$schema" from the top-level "properties" so the top level
    //    is no longer a closed object (the "required" constraint on "$schema"
    //    remains and still enforces presence).
    if let Some(props) = raw.get_mut("properties").and_then(|p| p.as_object_mut()) {
        props.remove("$schema");
        // If "properties" is now empty, remove the key entirely so the
        // closed-object check has no foothold.
        if props.is_empty() {
            raw.as_object_mut().map(|m| m.remove("properties"));
        }
    }

    // 2. Inject "$schema": {"type":"string"} into each oneOf branch that
    //    declares "properties", making the branch's closed-object shape include
    //    the field that was previously only at the outer level.
    if let Some(branches) = raw.get_mut("oneOf").and_then(|v| v.as_array_mut()) {
        for branch in branches.iter_mut() {
            if let Some(props) = branch.get_mut("properties").and_then(|p| p.as_object_mut()) {
                props
                    .entry("$schema")
                    .or_insert_with(|| serde_json::json!({"type": "string"}));
            }
        }
    }

    serde_json::from_value(raw).expect("schema round-trips after oneOf branch injection")
}

fn verbs() -> Vec<Verb> {
    vec![
        Verb {
            name: "status",
            summary: "per-repo workspace state (branch, tip, lock, relation)",
            schema: Some(schema_status),
            schema_record: None,
        },
        Verb {
            name: "doctor",
            summary: "convention-violation checks (orphans, drift, stale locks)",
            schema: Some(schema_doctor),
            schema_record: None,
        },
        Verb {
            name: "sync",
            summary: "bring another workspace's committed state into this one",
            schema: Some(schema_sync),
            schema_record: Some(schema_sync_record),
        },
        Verb {
            name: "sync-to",
            summary: "advance target workspace to CWD's tip (3-step orchestration: rebase, relock, FF-advance)",
            schema: Some(schema_sync_to),
            // sync-to NDJSON records share the same per-repo wire shape as sync
            // records (both flatten `SyncOutcomeOutput` with `$schema`), so
            // `sync-to-record.json` reuses the same generated schema.
            schema_record: Some(schema_sync_record),
        },
        Verb {
            name: "push",
            summary: "publish member repos then the project repo to shared remotes",
            schema: Some(schema_push),
            schema_record: Some(schema_push_record),
        },
        Verb {
            name: "fetch",
            summary: "clone a project, or align its repos to rwv.lock without advancing pins",
            schema: Some(schema_fetch),
            schema_record: Some(schema_fetch_record),
        },
        Verb {
            name: "update",
            summary: "advance each repo to its branch HEAD on the remote and re-snapshot the lock",
            schema: Some(schema_update),
            schema_record: Some(schema_update_record),
        },
        Verb {
            name: "prime",
            summary: "agent-oriented orientation context for the workspace",
            schema: None,
            schema_record: None,
        },
        Verb {
            name: "explain",
            summary: "per-verb JIT reflection (this verb)",
            schema: None,
            schema_record: None,
        },
        Verb {
            name: "workweave",
            summary: "create, delete, or list workweaves for a project",
            schema: None,
            schema_record: None,
        },
        Verb {
            name: "abort",
            summary: "roll an in-flight sync or sync-to back to its savepoints, from either workspace it touched",
            schema: None,
            schema_record: None,
        },
        Verb {
            name: "add",
            summary: "clone a repo and register it in the active project manifest",
            schema: None,
            schema_record: None,
        },
        Verb {
            name: "remove",
            summary: "remove a repo from the active project manifest",
            schema: None,
            schema_record: None,
        },
        Verb {
            name: "role",
            summary: "change a repo's role in the active project manifest and settle its files",
            schema: None,
            schema_record: None,
        },
        Verb {
            name: "lock",
            summary: "snapshot current repo HEADs into rwv.lock (pure local; no network)",
            schema: None,
            schema_record: None,
        },
        Verb {
            name: "activate",
            summary: "set the active project, create symlinks, run integration install hooks",
            schema: None,
            schema_record: None,
        },
        Verb {
            name: "materialize",
            summary: "settle drift, clean up disabled integrations, repair weave-root symlinks, and run integration install hooks for the presented project, without selecting anything",
            schema: None,
            schema_record: None,
        },
        Verb {
            name: "init",
            summary: "create a new project (or adopt an existing repo) and auto-activate it",
            schema: None,
            schema_record: None,
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

/// Collect every file under `dir` (non-recursively) that passes `filter`.
///
/// The directory is silently skipped when it does not exist so callers that
/// add a new scripting directory do not need a path-existence guard.
fn collect_files_in_dir<F>(dir: &Path, filter: F) -> Vec<PathBuf>
where
    F: Fn(&Path) -> bool,
{
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    entries
        .into_iter()
        .filter_map(|e| {
            let path = e.path();
            (path.is_file() && filter(&path)).then_some(path)
        })
        .collect()
}

/// Collect all `.sh` files under `dir` recursively.
///
/// Shell scripts are prose-bearing shipped artifacts: their comments reach
/// contributors and operators the same way source comments do, and a consumer
/// word or tracker ID in a script comment is just as unfollowable from a
/// standalone clone as one in `src/`. The directory is silently skipped when it
/// does not exist.
fn collect_sh_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_sh_files_inner(dir, &mut out);
    out
}

fn collect_sh_files_inner(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_sh_files_inner(&path, out);
        } else if path.extension().and_then(OsStr::to_str) == Some("sh") {
            out.push(path);
        }
    }
}

/// Collect all `.yml` files directly under `dir` (non-recursively).
///
/// Used for `.github/workflows/`, where all workflow files sit at one depth.
/// The directory is silently skipped when it does not exist.
fn collect_yml_files_in_dir(dir: &Path) -> Vec<PathBuf> {
    collect_files_in_dir(dir, |p| {
        p.extension().and_then(OsStr::to_str) == Some("yml")
    })
}

/// Shipped prose-bearing artifacts outside the `src/`, `docs/*.md`, and `tests/`
/// scope that `src_docs_and_tests_files` already reaches.
///
/// Concretely:
///
/// - `scripts/` — all files (currently `*.sh`; new files land here automatically)
/// - `.githooks/` — all files (currently `commit-msg`, `pre-push`; hooks have no
///   extension, so an extension filter would miss them)
/// - `.github/workflows/*.yml` — GitHub Actions workflow YAML files; these carry
///   prose in `name:` and `run:` fields and are shipped as part of the repository
/// - `docs/*.sh` — any shell scripts directly in `docs/` (currently `install.sh`,
///   the user-facing installer script served at the docs site URL)
/// - `python/scripts/*.sh` — wheel-build and release scripts under `python/`
/// - `tests/*.sh` — shell-based end-to-end tests under `tests/` (the `.rs` files
///   in that directory are already in scope via `src_docs_and_tests_files`; these
///   are the sibling scripts)
///
/// This function does NOT apply a self-file exemption; its callers apply one if
/// needed. None of these file types can spell `CONSUMER_VOCABULARY` or
/// `FOREIGN_VOCABULARY` without triggering the rule they're being checked against,
/// so no new exemption is required for the files in this set.
fn shipped_prose_auxiliary_files(root: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();

    // scripts/ — all files, no extension filter
    let scripts_dir = root.join("scripts");
    if let Ok(rd) = std::fs::read_dir(&scripts_dir) {
        let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            if path.is_file() {
                files.push(path);
            }
        }
    }

    // .githooks/ — all files, no extension filter (hooks have none)
    let hooks_dir = root.join(".githooks");
    if let Ok(rd) = std::fs::read_dir(&hooks_dir) {
        let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            if path.is_file() {
                files.push(path);
            }
        }
    }

    // .github/workflows/*.yml — GitHub Actions workflow YAML
    files.extend(collect_yml_files_in_dir(&root.join(".github/workflows")));

    // docs/*.sh — shell scripts directly in docs/ (not recursing; the .md files
    // in docs/ are already reached by src_docs_and_tests_files via collect_md_files)
    files.extend(collect_files_in_dir(&root.join("docs"), |p| {
        p.extension().and_then(OsStr::to_str) == Some("sh")
    }));

    // python/scripts/*.sh — wheel-build and release scripts
    files.extend(collect_sh_files(&root.join("python/scripts")));

    // tests/*.sh — shell-based e2e tests (sibling to tests/*.rs which is already in scope)
    files.extend(collect_files_in_dir(&root.join("tests"), |p| {
        p.extension().and_then(OsStr::to_str) == Some("sh")
    }));

    files.sort();
    files
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
        let canonical = repoweave::path_spelling::resolve(&path).unwrap_or_else(|_| path.clone());
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
        let canonical = repoweave::path_spelling::resolve(&p).unwrap_or(p);
        exclude_dirs.push(canonical);
    }

    // Assembled-output directories: rustdoc-leak detection applies here.
    let mut assembled_dirs: Vec<PathBuf> = Vec::new();
    for asm_rel in &["reference/explain", "reference/prime"] {
        let p = docs_dir.join(asm_rel);
        let canonical = repoweave::path_spelling::resolve(&p).unwrap_or(p);
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
                    "link-check: could not read {}: {e:#}",
                    md_path.display()
                ));
                continue;
            }
        };

        // Determine whether this file is in an assembled-output directory.
        let canonical_file =
            repoweave::path_spelling::resolve(md_path).unwrap_or_else(|_| md_path.clone());
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

/// Verify that no assembled explain page tells the reader to run
/// `rwv --help <verb>` for a subcommand's own help.
///
/// Clap resolves `--help` before it reads the rest of the argument list, so
/// that spelling prints `rwv`'s top-level help rather than the named verb's.
/// The verb's own help needs `--help` placed after it: `rwv <verb> --help`.
/// Template directories are excluded; checking the assembled output is
/// equivalent, since the generator copies this text through unchanged, and a
/// generated page is what a reader (or an agent run via `rwv explain`)
/// actually sees.
fn check_help_flag_ordering(docs_dir: &Path) -> Vec<String> {
    let explain_dir = docs_dir.join("reference/explain");
    let template_dir = explain_dir.join("templates");
    let wrong_order = Regex::new(r"rwv --help [a-z][a-z0-9-]*").unwrap();

    let mut findings = Vec::new();
    for page in collect_md_files(&explain_dir, &[template_dir]) {
        let content = fs::read_to_string(&page).unwrap_or_default();
        for (lineno, line) in content.lines().enumerate() {
            if let Some(m) = wrong_order.find(line) {
                findings.push(format!(
                    "help-flag-ordering: {}:{}: `{}` prints rwv's top-level help, not the \
                     verb's (clap resolves --help before the subcommand) — write \
                     `rwv <verb> --help`",
                    page.display(),
                    lineno + 1,
                    m.as_str()
                ));
            }
        }
    }
    findings
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

    // "creation_params_table": the `rwv add --new` per-registry creation
    // parameter table, walked from `builtin_registries()` — see
    // `creation_params_table_md`.
    m.insert("creation_params_table", creation_params_table_md());

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
    let mut out = String::from("# rwv explain — index\n\n");
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
        "`rwv explain <token>` serves the entry for a refusal token, a `rwv doctor` \
         finding kind, or a VCS error kind.\n",
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
            "cannot read coverage allowlist at {}: {e:#}",
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
        .map_err(|e| anyhow::anyhow!("cannot read cli.md at {}: {e:#}", cli_md_path.display()))?;

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
            "cannot read env-input allowlist at {}: {e:#}",
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
                errors.push(format!(
                    "env-input: could not read {}: {e:#}",
                    path.display()
                ));
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
                errors.push(format!(
                    "vcs-seam: could not read {}: {e:#}",
                    path.display()
                ));
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
        workspace: repoweave::path_spelling::WirePath::of(std::path::Path::new(
            "/sentinel/workspace",
        )),
        workweave: Some("sentinel-project--sentinel-ww".to_owned()),
        workweave_unregistered: true,
        project: repoweave::manifest::ProjectName::new("sentinel-project").unwrap(),
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
            "cannot read plugin-protocol.md at {}: {e:#}",
            protocol_md_path.display()
        )
    })?;
    Ok(check_envelope_output_documented(&protocol_md_content))
}

/// The library crate's name as source spells it.
const LIBRARY_CRATE: &str = "repoweave";

/// Check that no file under `plugin_dir` reaches the library: no path through
/// `library_crate` (`library_crate::…`, `use library_crate`, `extern crate
/// library_crate`), and no `#[path]` or `include!`-family macro, the two ways
/// a binary compiles another file's source into itself without naming its
/// crate. The crate's name in prose or in a URL is not a reference and is not
/// reported. A missing `plugin_dir` is an error, not a pass.
///
/// Returns error messages (one per offending line); empty means clean.
fn check_plugin_library_boundary(plugin_dir: &Path, library_crate: &str) -> Vec<String> {
    if !plugin_dir.is_dir() {
        return vec![format!(
            "plugin-boundary: {} is not a directory; the check has nothing to read",
            plugin_dir.display()
        )];
    }
    let name = regex::escape(library_crate);
    let crate_token = Regex::new(&format!(
        r"\b{name}\s*::|\buse\s+(?:::\s*)?{name}\b|\bextern\s+crate\s+{name}\b"
    ))
    .unwrap();
    let source_inclusion = Regex::new(r"#\s*\[\s*path\b|\binclude(?:_str|_bytes)?!").unwrap();
    let mut errors = Vec::new();
    for path in collect_rs_files(plugin_dir) {
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                errors.push(format!(
                    "plugin-boundary: could not read {}: {e:#}",
                    path.display()
                ));
                continue;
            }
        };
        for (n, line) in content.lines().enumerate() {
            let what = if crate_token.is_match(line) {
                format!("names the `{library_crate}` library")
            } else if source_inclusion.is_match(line) {
                "compiles in source from another file".to_owned()
            } else {
                continue;
            };
            errors.push(format!(
                "plugin-boundary: {}:{}: {what}: {}",
                path.display(),
                n + 1,
                line.trim()
            ));
        }
    }
    errors
}

/// Every retired-scheme ID spelling this gate matches: the literal prefix, the
/// slug lengths that follow it, and the separator its numeric sub-IDs use.
///
/// The retired tracker prefix appears in two spellings of one ID. `fo-` is how
/// prose writes it; `fo_` is the only spelling a Rust module or function name
/// can carry. A numeric sub-ID follows whichever separator the prefix itself
/// uses, so the two spellings of one ID differ in their separators and in
/// nothing else. The third entry is the retired doc-claim scheme, whose slugs
/// run as short as three characters and so take no four-character floor; its
/// seventeen-character prefix is what lets it take the wider slug range
/// without collecting anything else.
///
/// No entry here may be written out in full anywhere in this file: the gate
/// reads its own source, so an illustrative ID in a doc comment is a finding.
///
/// `rwv-`, the prefix the tracker issues now, is deliberately absent and stays
/// caught by whoever is reading — see `check_no_tracker_ids`.
const RETIRED_ID_SPELLINGS: &[(&str, RangeInclusive<usize>, u8)] = &[
    ("fo-", 4..=8, b'.'),
    ("fo_", 4..=8, b'_'),
    ("project-reporoot-", 1..=8, b'.'),
];

/// Leftmost review-record shorthand on `line`: the word "correction"
/// immediately followed by a number, at a word boundary,
/// case-insensitive.
///
/// That shorthand names an entry in a design record the reader does not have,
/// which is what a tracker ID does. The number is required, so the ordinary
/// word ("a correction to the lock") is never matched. Measured over every
/// tracked file in the repository, the pattern reports only the shorthand.
fn find_review_shorthand(line: &str) -> Option<String> {
    const WORD: &str = "correction";
    let lower = line.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    lower.match_indices(WORD).find_map(|(i, _)| {
        let before_ok = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        let mut j = i + WORD.len();
        while j < bytes.len() && bytes[j] == b' ' {
            j += 1;
        }
        let digits_from = j;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        (before_ok && j > digits_from).then(|| line[i..j].to_string())
    })
}

/// Match a retired-scheme ID at a word boundary, reporting the leftmost one on
/// the line so the finding names the token a reader scanning left to right hits
/// first.
///
/// Deliberately hand-rolled rather than regex: the generator has no regex
/// dependency, and the shape is narrow enough to scan directly.
fn find_tracker_id(line: &str) -> Option<String> {
    RETIRED_ID_SPELLINGS
        .iter()
        .filter_map(|(prefix, slug_lens, sub_id_sep)| {
            find_prefixed_id(line, prefix, slug_lens, *sub_id_sep)
        })
        .min()
        .map(|(_, id)| id)
}

/// Leftmost `prefix` on `line` followed by a `slug_lens`-long run of lowercase
/// letters and digits at a word boundary, as `(offset, token)`.
///
/// The character before the prefix must be neither alphanumeric nor `_`.
/// Excluding `_` is what makes this a boundary in identifier space rather than
/// in prose: without it every `tracker_id_check_..._fo_prefix...` name in this
/// file is a finding, and a gate that reports its own test names gets turned
/// off.
fn find_prefixed_id(
    line: &str,
    prefix: &str,
    slug_lens: &RangeInclusive<usize>,
    sub_id_sep: u8,
) -> Option<(usize, String)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while let Some(off) = line[i..].find(prefix) {
        let start = i + off;
        let prev_ok =
            start == 0 || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
        let mut end = start + prefix.len();
        while end < bytes.len() && (bytes[end].is_ascii_lowercase() || bytes[end].is_ascii_digit())
        {
            end += 1;
        }
        let slug_len = end - start - prefix.len();
        if prev_ok && slug_lens.contains(&slug_len) {
            // Include a trailing numeric sub-ID so the reported token matches
            // what the author wrote.
            let mut tail = end;
            if tail < bytes.len() && bytes[tail] == sub_id_sep {
                let mut d = tail + 1;
                while d < bytes.len() && bytes[d].is_ascii_digit() {
                    d += 1;
                }
                if d > tail + 1 {
                    tail = d;
                }
            }
            return Some((start, line[start..tail].to_owned()));
        }
        i = start + prefix.len();
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

/// `.toml` files directly in `root` — `Cargo.toml` foremost, where a
/// dependency comment is exactly the kind of prose a tracker ID gets written
/// into.
fn root_level_toml_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("toml") {
            out.push(path);
        }
    }
    out
}

/// Scan `src/`, `docs/`, `tests/`, root-level `.md`/`.rs`/`.toml` files, and
/// shipped prose-bearing auxiliary files for tracker IDs.
///
/// **What is matched** is `RETIRED_ID_SPELLINGS`: the retired tracker prefix
/// both as prose writes it and as a Rust identifier has to write it, and the
/// retired doc-claim prefix. All three are dead schemes — nothing is issued
/// under them — so a hit is old text coming back rather than a new author's
/// habit. They were admitted on a measurement: over the file set this gate
/// reads, and over every tracked file in the repository, the three together
/// report nothing that is not an ID.
///
/// **What is not matched, and why.** `rwv-`, the prefix the tracker issues
/// now, is also this repo's own vocabulary namespace: matched on the retired
/// prefix's own terms it collides with roughly 1,300 occurrences across the
/// file set this gate reads, some 290 of them in `src/*.rs` alone, led by
/// `rwv-active`, `rwv-ours` and `rwv-owned`. A matcher that reports correct
/// code gets turned off, so `rwv-` stays caught by whoever is reading, and
/// **a green gate means no retired-scheme ID, not no tracker ID.** Widening to
/// a live prefix needs a suppression mechanism this gate does not have.
///
/// Code is ground truth and architecture docs carry rationale; a tracker ID
/// in any of these surfaces points a reader at something they cannot open,
/// and it reaches users directly through `rwv explain` (whose pages are
/// `include_str!`'d from `docs/reference/explain/`) and through
/// `anyhow::bail!` text. `tests/` gets no carve-out: a test comment may name
/// the regression it pins in its own words, but not by an ID a standalone
/// cloner cannot look up — that provenance is what the commit that added the
/// test already carries. Root-level files (`CLAUDE.md`, `build.rs`,
/// `Cargo.toml`) get no carve-out either: `CLAUDE.md` is the file stating
/// this ban, and the reader most likely to imitate a tracker ID is one who
/// just read it there; `build.rs` is ordinary Rust that happens to sit
/// outside `src/`, not a different kind of surface; `Cargo.toml`'s
/// dependency comments are exactly the prose a tracker ID gets written into.
///
/// **Widened scope** (via `shipped_prose_auxiliary_files`): `scripts/` (any
/// file), `.githooks/` (any file), `.github/workflows/*.yml`, any `.sh`
/// directly in `docs/`, any `.sh` under `python/scripts/`, and any `.sh`
/// directly in `tests/`. A tracker ID in a shell comment is as unfollowable
/// from a standalone clone as one in `src/`. The same auxiliary-file set is
/// also added by `check_no_consumer_vocabulary` and `check_no_foreign_vocabulary`;
/// see `check_no_consumer_vocabulary`'s doc comment for the cross-gate rationale.
fn check_no_tracker_ids(root: &Path) -> Vec<String> {
    let mut errors = Vec::new();
    let mut files = src_docs_and_tests_files(root);
    files.extend(root_level_md_files(root));
    files.extend(root_level_rs_files(root));
    files.extend(root_level_toml_files(root));
    files.extend(shipped_prose_auxiliary_files(root));
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
            if let Some(token) = find_review_shorthand(line) {
                errors.push(format!(
                    "{rel}:{}: review-record shorthand `{token}`",
                    n + 1
                ));
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
/// The scan tracks `"` (honoring backslash escapes) so the `//` inside a URL
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
    /// The block's lines, with a citation broken across a line break closed
    /// back up onto the line it starts on.
    ///
    /// A reader reassembles a wrapped path without noticing. A scan that reads
    /// one line at a time sees two fragments, and neither is the citation: the
    /// head is a path prefix with no filename, the tail is a filename with no
    /// path. Every finding a wrapped citation produces is therefore attributed
    /// to the line the citation **starts** on — the line an author edits to fix
    /// it — and the tail line reports nothing.
    ///
    /// The head must contain a `/` for the break to close. Without that guard
    /// an ordinary wrapped sentence joins: the last word of one line and a
    /// filename opening the next would fuse into a token nobody wrote, and the
    /// gate would report a citation it invented.
    fn rejoined(&self) -> Vec<(usize, String)> {
        let mut lines = self.lines.clone();
        for i in (1..lines.len()).rev() {
            let head = trailing_path_run(&lines[i - 1].1);
            let tail = leading_path_run(&lines[i].1);
            if !head.contains('/')
                || tail.is_empty()
                || !is_doc_token(&format!("{head}{}", tail.trim_end_matches('.')))
            {
                continue;
            }
            let (moved, rest) = lines[i].1.split_at(tail.len());
            let (moved, rest) = (moved.to_owned(), rest.to_owned());
            lines[i - 1].1.push_str(&moved);
            lines[i].1 = rest;
        }
        lines
    }

    fn text(&self) -> String {
        self.rejoined()
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
/// alone. A line break splits a run, so a caller scanning comments feeds this
/// `CommentBlock::rejoined`'s lines rather than the file's.
fn doc_path_tokens(text: &str) -> Vec<&str> {
    text.split(|c: char| !is_path_char(c))
        .map(|run| run.trim_end_matches('.'))
        .filter(|tok| is_doc_token(tok))
        .collect()
}

fn is_path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-')
}

/// True if `token` is what `doc_path_tokens` keeps: a path whose last
/// component is `<stem>.<ext>` with a non-empty `stem` and `ext` in
/// `DOC_PATH_EXTENSIONS`.
fn is_doc_token(token: &str) -> bool {
    token
        .rsplit('/')
        .next()
        .and_then(|last| last.rsplit_once('.'))
        .is_some_and(|(stem, ext)| !stem.is_empty() && DOC_PATH_EXTENSIONS.contains(&ext))
}

fn leading_path_run(text: &str) -> &str {
    text.split(|c: char| !is_path_char(c)).next().unwrap_or("")
}

fn trailing_path_run(text: &str) -> &str {
    text.rsplit(|c: char| !is_path_char(c)).next().unwrap_or("")
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
/// ends the region exactly as `mod tests` does. A boundary that recognized one
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
/// genuinely operates on the file is the case being exempted. The basename
/// match `check_doc_citations` runs against this set widens the hole
/// slightly further: a slashed citation naming a real document could go
/// unreported if some unrelated operated file happens to share its
/// basename. Measured as small in practice — the fixture names
/// `tests_file_doc_filenames` exempts today are generic (a shared notes
/// file, per-workweave feature notes) and none collides with a real
/// document's basename under `docs`.
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

/// `src_code_doc_filenames`'s counterpart for `tests/`, **for one test file**:
/// every document filename that file builds as fixture content — the last
/// component of each path-shaped token it writes as a string literal, outside
/// a comment.
///
/// This is what makes a fixture path exempt rather than allowlisted: a test
/// that writes a file into a temp tree and a comment nearby that names the
/// same file are naming the same fact, the way
/// `src_code_doc_filenames` already treats `include_str!(...)` and the
/// comment describing it. `before_test_module` is applied for the same
/// reason it is on the `src/` side — a `tests/*.rs` file rarely carries a
/// `#[cfg(test)] mod tests { ... }` block of its own (the whole file is
/// already test-only), so this is a no-op there, but a file that does
/// nest one keeps its fixture-vouching-for-itself guard.
///
/// One file at a time is the scope, and that is the difference from the
/// `src/` side rather than an implementation detail. What `src/` code operates
/// on is a fact about the program, true wherever a comment names it; what a
/// fixture writes is a fact about that fixture, and says nothing about a
/// comment in the test next door.
///
/// The residue this does not tell apart: any non-comment occurrence counts,
/// including a string that only mentions a filename in prose rather than
/// operating on it — an audit justification quoting a document by name reads
/// exactly like a fixture writing that name to disk. That is a wider hole
/// than `src_code_doc_filenames`'s, and it is not hypothetical:
/// `tests/destructive_ops_audit_test.rs` used to carry a bare mention of
/// `docs/internals/branch-model.md`'s filename inside `justification` string
/// literals. Cleared by rewriting those strings rather than by narrowing this
/// function, because the narrower fix (matching only arguments to a known
/// write call) would need to parse Rust rather than scan text, and would need
/// to be redone the next time a fixture is built a new way. What the scope
/// above bounds is how far one such mention reaches: its own file, not every
/// citation in `tests/`.
fn tests_file_doc_filenames(path: &Path) -> HashSet<String> {
    let mut names = HashSet::new();
    let Ok(content) = fs::read_to_string(path) else {
        return names;
    };
    for line in before_test_module(&content).lines() {
        for token in doc_path_tokens(code_on_line(line)) {
            names.insert(token_filename(token).to_owned());
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
/// - a filename the code itself operates on (`src_code_doc_filenames`,
///   `tests_file_doc_filenames`) is exempt **by basename**, slashed token or
///   bare: a comment naming a file by its bare name and a fixture writing
///   that same name under a directory are naming the same fact, so the
///   exemption checks `token_filename`, not the token's own shape. A bare
///   filename is also
///   satisfied by a resolving path to the same filename **in the same
///   comment block** — which is what a markdown link written `[name](path)`
///   already gives the reader, and what the paragraph above does for its own
///   example. That second form is block-level, not file-level: a resolving
///   path elsewhere in the file is not something a reader of this comment
///   has, and it does not extend to slashed tokens — a fixture path already
///   has the operated-set exemption for that.
///
/// # This gate is not the whole rule
///
/// `CLAUDE.md` bans three shapes in comments. The tracker-ID shape is
/// `check_no_tracker_ids`. The third — a **section pointer** into a design
/// document, a document name followed by a section token — is still not
/// mechanized *as such*. What changed is the size of the gap, not its
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
/// This file keeps the exclusion, and the reason does not generalize to any
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
            let lines = block.rejoined();
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
            for (i, (n, text)) in lines.iter().enumerate() {
                if i > 0 && is_local_ref_hatch(&lines[i - 1].1) {
                    continue;
                }
                for token in doc_path_tokens(text) {
                    let bare = !token.contains('/');
                    if (bare && !token.ends_with(".md"))
                        || operated.contains(token_filename(token))
                        || (bare && shown_as_path.contains(token))
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

/// The `.rs` half of `src_and_docs_files` — the scope of `check_doc_symbol_refs`,
/// and half the scope of `check_doc_citations` (see `run_doc_citation_check`,
/// which adds `tests/` on top).
///
/// Both rules govern comments under `src/`, so the `.md` files that shared
/// scope also yields are filtered out.
fn src_rs_files(root: &Path) -> Vec<PathBuf> {
    src_and_docs_files(root)
        .into_iter()
        .filter(|p| p.extension() == Some(OsStr::new("rs")))
        .collect()
}

/// `check_doc_citations` over `src/` and `tests/` together — the citation
/// rule does not carve `tests/` out, unlike `check_doc_symbol_refs`, which
/// stays on `src_rs_files` alone.
///
/// The exemption a file is checked against is what its own reader can see:
/// `src/`'s `include_str!`s and the like everywhere, and a fixture path only
/// in the `tests/*.rs` file that writes it. Each test file therefore gets its
/// own call rather than one call against a set the whole tree contributed to.
fn run_doc_citation_check(root: &Path) -> Vec<String> {
    let operated = src_code_doc_filenames(root);
    let mut errors = check_doc_citations(root, &src_rs_files(root), &operated);
    for path in collect_rs_files(&root.join("tests")) {
        let mut exempt = operated.clone();
        exempt.extend(tests_file_doc_filenames(&path));
        errors.extend(check_doc_citations(
            root,
            std::slice::from_ref(&path),
            &exempt,
        ));
    }
    errors
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
/// and this list is not a log of past violations. An inflection can be its
/// own entry when it is itself house vocabulary: `choreographer` is listed
/// literally because unstemmed matching of `choreograph` does not cover it. `workweave` is core rwv
/// vocabulary and must never be added here, and neither is `dispatch` — rwv's
/// own CLI dispatch uses the word throughout `src/`, so it cannot be added as
/// a bare word without banning rwv's own vocabulary alongside the consumer
/// sense of it.
const CONSUMER_VOCABULARY: &[&str] = &[
    "bead",
    "choreograph",
    "choreographer",
    "subagent",
    "sling",
    "tl",
    "epic",
];

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

/// Scan `src/`, `docs/`, `tests/`, and shipped prose-bearing auxiliary files for
/// `CONSUMER_VOCABULARY`, at word boundaries.
///
/// **Core scope** (via `src_docs_and_tests_files`): `src/`, all `.md` under
/// `docs/`, all `.rs` under `tests/`, and `docs/env-input-allowlist.txt`. This
/// is the words-not-IDs sibling of `check_no_tracker_ids` and shares the same
/// core scope.
///
/// **Widened scope** (via `shipped_prose_auxiliary_files`): `scripts/` (any
/// file), `.githooks/` (any file), `.github/workflows/*.yml`, any `.sh` directly
/// in `docs/`, any `.sh` under `python/scripts/`, and any `.sh` directly in
/// `tests/`. These are shipped prose-bearing artifacts the core collectors do not
/// reach; the class of silent violation this gate exists to prevent is exactly one
/// that lived in `scripts/ci-local.sh`.
///
/// **No `tests/` exemption**: a consumer word is a dead end for a standalone
/// cloner in a test comment exactly as much as in `src/`.
///
/// **Self-file exemption**: `src/bin/generate-explain.rs` is skipped. It defines
/// `CONSUMER_VOCABULARY` and must spell each word out literally to check for it,
/// so scanning it here would have the gate flag its own definition.
/// `check_no_foreign_vocabulary` gets the same exception for free — `FOREIGN_VOCABULARY`
/// lives in this same file, just outside that gate's docs-only scope. No file
/// in the widened scope spells the banned words for definitional reasons, so no
/// additional exemption is needed there.
///
/// **Sibling scope decisions** (recorded here as the cross-gate canonical reference):
///
/// - `check_no_tracker_ids` is also widened to cover the same auxiliary files —
///   a tracker ID in a script comment is equally unfollowable from a standalone
///   clone, and the two gates share the rationale. See its doc comment.
/// - `check_no_foreign_vocabulary` is also widened — foreign deployment words
///   (`rig`, `gas city`, etc.) do not belong in any shipped artifact regardless
///   of whether that artifact lands on the `rwv explain` surface; the vocabulary
///   exclusion is a source-level cleanliness rule, not an operator-surface rule.
///   See its doc comment.
fn check_no_consumer_vocabulary(root: &Path) -> Vec<String> {
    let self_path = Path::new("src/bin/generate-explain.rs");
    let mut files = src_docs_and_tests_files(root);
    files.extend(shipped_prose_auxiliary_files(root));
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
/// neighbors — not just `rig`, where `origin`, `right` and `trigger` all
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

/// Scan `docs/` and shipped prose-bearing auxiliary files for vocabulary from a
/// specific deployment of rwv.
///
/// **Original scope**: `docs/` (all `.md` files). `src/prime.rs` already asserts
/// `rwv prime` stays free of deployment vocabulary, but that test only sees the
/// prime overview. `rwv explain` prints `docs/reference/explain/*.md` verbatim
/// via `include_str!`, and one such page shipped "the Gas City rig's standard …"
/// to every user. Scanning the whole of `docs/` covers the templates, the
/// assembled pages, and the published explanation joints.
///
/// **Widened scope**: also covers `scripts/` (any file), `.githooks/` (any file),
/// `.github/workflows/*.yml`, any `.sh` in `docs/`, any `.sh` under
/// `python/scripts/`, and any `.sh` in `tests/` — the same `shipped_prose_auxiliary_files`
/// set that `check_no_consumer_vocabulary` and `check_no_tracker_ids` add.
/// Foreign deployment words (`rig`, `gas city`, etc.) do not belong in any
/// shipped artifact regardless of whether it reaches the `rwv explain` surface;
/// the exclusion is a source-level cleanliness rule, not purely an
/// operator-surface rule. Keeping the three sibling gates' auxiliary-file scopes
/// identical prevents silent divergence.
///
/// `src/` and `tests/` are deliberately outside this gate's scope. The foreign
/// vocabulary enters through prose-facing artifacts, not through code; scanning
/// identifiers and type names in `src/` would report false positives (a function
/// named `trigger` contains "rig"). The consumer-vocabulary gate's broader `src/`
/// scope is appropriate there because consumer words are entirely alien to rwv's
/// domain model, whereas "rig" alone is a common English word that can appear
/// legitimately in code.
///
/// **Sibling scope decision**: see `check_no_consumer_vocabulary`'s doc comment
/// for the cross-gate canonical reference.
fn check_no_foreign_vocabulary(root: &Path) -> Vec<String> {
    let mut errors = Vec::new();
    let mut files: Vec<PathBuf> = collect_md_files(&root.join("docs"), &[]);
    files.extend(shipped_prose_auxiliary_files(root));
    for path in files {
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

/// Scan operator surfaces for `docs/internals/` paths.
///
/// The reader-axis rule is that operator-facing text references only pages
/// mdBook renders. `docs/internals/` is not in `docs/SUMMARY.md`, so a
/// reader who meets one of those paths on an operator surface has nothing to
/// open. The generator is an audience boundary — comment text that would be
/// legal in a clone becomes an operator surface once it is lifted onto one,
/// and the check makes that boundary executable.
///
/// Scope is the union of two populations: the two directories the generator
/// writes (assembled `rwv explain` pages and their embedded schemas —
/// templates under `docs/reference/explain/templates/` are excluded, since
/// the assembled output is what a user reads and what this gate reports on),
/// and every page `docs/SUMMARY.md` links to, derived by parsing that file's
/// markdown links rather than hand-enumerated, so a page added to the
/// sidebar enters the scan without a matching edit here.
///
/// Root `README.md` and `ARCHITECTURE.md` are out of scope even though they
/// are operator-facing: they are not listed in `docs/SUMMARY.md`, are read
/// on GitHub directly rather than through mdBook, and `docs/internals/`
/// links resolve there the same as any other repo-relative link — both
/// already do this deliberately.
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

    let summary_pages = collect_summary_listed_pages(root);
    if summary_pages.is_empty() {
        errors.push(format!(
            "operator-surface scan: {} yielded zero pages — the scan population \
             must not be empty",
            root.join("docs/SUMMARY.md").display()
        ));
    }
    files.extend(summary_pages);

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

/// Every page `docs/SUMMARY.md` links to, resolved against `docs/`.
///
/// Parses markdown links `[text](target)` anywhere in the file; a target
/// that is an anchor only, an absolute URL, or does not end in `.md` is not
/// a page and is skipped. Paths are resolved relative to `docs/` — the
/// directory `SUMMARY.md` lives in and the base mdBook resolves its own
/// links against — and are not checked for existence: a listed page that is
/// missing on disk is a defect `check_assembled_docs`' link-cleanliness scan
/// reports, not one this walk silently drops by skipping it. Returns an
/// empty vec if `docs/SUMMARY.md` cannot be read.
fn collect_summary_listed_pages(root: &Path) -> Vec<PathBuf> {
    let docs_dir = root.join("docs");
    let Ok(content) = fs::read_to_string(docs_dir.join("SUMMARY.md")) else {
        return Vec::new();
    };
    let link_re = Regex::new(r"\[([^\]]*)\]\(([^)]+)\)").unwrap();
    let mut pages: HashSet<PathBuf> = HashSet::new();
    for cap in link_re.captures_iter(&content) {
        let target = &cap[2];
        if target.starts_with('#') || target.contains("://") {
            continue;
        }
        let path_part = target.split('#').next().unwrap_or(target);
        if !path_part.ends_with(".md") {
            continue;
        }
        let path_part = path_part.strip_prefix("./").unwrap_or(path_part);
        pages.insert(docs_dir.join(path_part));
    }
    let mut pages: Vec<PathBuf> = pages.into_iter().collect();
    pages.sort();
    pages
}

/// A phrase a house prose rule bans, and what to write instead.
struct BannedTerm {
    /// A regex body, matched case-insensitively between word boundaries.
    pattern: &'static str,
    /// What the rule asks for in its place.
    write: &'static str,
    /// Words that, standing just before a match, make it another tool's own
    /// term rather than a synonym of ours.
    unless_after: &'static [&'static str],
}

/// Synonyms the canonical-vocabulary rule in `CLAUDE.md` replaces.
///
/// Whitespace inside a phrase matches any run of it, line breaks included,
/// so a phrase wrapped across two lines of a page is still one phrase.
///
/// `manifest repository` is not banned: the migration guide names
/// `git-repo`'s manifest repository, that tool's term for its own composition
/// repo, and a matcher that reported it would report correct prose.
const BANNED_SYNONYMS: &[BannedTerm] = &[
    BannedTerm {
        pattern: r"manifest(?:\s+|-)repos?",
        write: "member repo",
        unless_after: &[],
    },
    BannedTerm {
        pattern: r"constituent\s+repos?",
        write: "member repo",
        unless_after: &[],
    },
    BannedTerm {
        pattern: r"weave\s+director(?:y|ies)",
        write: "weave root",
        unless_after: &[],
    },
    BannedTerm {
        pattern: r"workspace\s+roots?",
        write: "weave root",
        unless_after: ECOSYSTEM_ROOT_QUALIFIERS,
    },
];

/// Words before `workspace root` that make it an ecosystem's workspace root —
/// the top-level manifest of a Cargo or uv workspace — rather than a weave's.
/// `multiple` is Cargo's own error text, which the cargo-workspace page quotes
/// verbatim: `multiple workspace roots found in the same workspace`.
const ECOSYSTEM_ROOT_QUALIFIERS: &[&str] = &[
    "cargo", "uv", "npm", "pnpm", "nested", "virtual", "multiple",
];

/// Spellings the American-spelling rule in `CLAUDE.md` replaces, one word
/// family per entry.
///
/// A family is a closed list of stems, not a suffix rule: a bare `-ise`
/// matcher reports `otherwise`, `premise` and `exercise`, and a bare `-our`
/// matcher reports `hour` and `detour`. The `-is-` stems carry their suffixes
/// explicitly, so `organism`, `synthesis`, `mechanism` and `criticism` stay
/// clear, and the `-ys-` family leaves out `-es`, which is also the plural of
/// `analysis`.
///
/// Regex `\b` counts `_` as a word character, so a stem inside a snake_case
/// identifier is not matched, and neither is one inside a CamelCase name,
/// whose next letter continues the word. Renaming a public identifier is a
/// break, so the gate holds prose and leaves names to review.
///
/// The spelling scan of this file blanks the table below; see
/// `masked_self_text`.
const BRITISH_SPELLINGS: &[BannedTerm] = &[
    BannedTerm {
        pattern: r"[a-z]*(?:behaviour|honour|neighbour|favour|flavour|labour|colour|armour|humour|rumour|vigour|harbour|odour|endeavour|savour|rigour|vapour)[a-z]*",
        write: "-or, not -our",
        unless_after: &[],
    },
    BannedTerm {
        pattern: r"(?:un|re|de|non|pre|mis|over|under)?(?:agon|amort|apolog|author|canonical|capital|categor|central|character|colon|commercial|critic|custom|econom|emphas|equal|external|familiar|fantas|fertil|final|formal|general|global|harmon|hypothes|industrial|initial|internal|item|jeopard|legal|local|material|maxim|mechan|memo|minim|mobil|modern|monopol|national|natural|neutral|normal|novel|optim|organ|ostrac|parameter|parenthes|patron|penal|personal|polar|priorit|privat|public|random|rational|real|recogn|regular|sanit|scrutin|serial|special|stabil|standard|steril|subsid|summar|symbol|sympath|synchron|synthes|tantal|terror|theor|token|trivial|util|vandal|vector|visual)is(?:e|ed|es|ing|er|ers|ation|ations|able|ability)",
        write: "-ize, not -ise",
        unless_after: &[],
    },
    BannedTerm {
        pattern: r"(?:anal|paral|catal|dial)ys(?:e|ed|ing|er|ers)",
        write: "-yze, not -yse",
        unless_after: &[],
    },
    BannedTerm {
        pattern: r"(?:un|re|mis)?(?:model|label|cancel|signal|travel|level|fuel|channel|tunnel|marshal|dial|total|equal|counsel|funnel|quarrel|rival|pedal)l(?:ed|ing|er|ers)",
        write: "one l before the suffix",
        unless_after: &[],
    },
    BannedTerm {
        pattern: r"judgements?",
        write: "judgment",
        unless_after: &[],
    },
    BannedTerm {
        pattern: r"artefacts?",
        write: "artifact",
        unless_after: &[],
    },
    BannedTerm {
        pattern: r"(?:catalogu|analogu)(?:e|es|ed|ing)",
        write: "-og, not -ogue",
        unless_after: &[],
    },
    BannedTerm {
        pattern: r"(?:defen|offen|licen|preten)ces?",
        write: "-se, not -ce",
        unless_after: &[],
    },
    BannedTerm {
        pattern: r"(?:centr|metr|litr|fibr|calibr)e[sd]?",
        write: "-er, not -re",
        unless_after: &[],
    },
    BannedTerm {
        pattern: r"programmes?",
        write: "program",
        unless_after: &[],
    },
    BannedTerm {
        pattern: r"grey(?:s|ed|er|est|ing|ish)?",
        write: "gray",
        unless_after: &[],
    },
    BannedTerm {
        pattern: r"whilst",
        write: "while",
        unless_after: &[],
    },
    BannedTerm {
        pattern: r"amongst",
        write: "among",
        unless_after: &[],
    },
];

/// Which of the two house prose rules a scanned text answers to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProseRules {
    VocabularyAndSpelling,
    Vocabulary,
    Spelling,
}

impl ProseRules {
    fn vocabulary(self) -> bool {
        self != Self::Spelling
    }

    fn spelling(self) -> bool {
        self != Self::Vocabulary
    }
}

/// One text the vocabulary-and-spelling gate reads, named the way a finding
/// against it is reported.
struct ProseText {
    name: String,
    text: String,
    rules: ProseRules,
}

/// `--help` as the operator reads it, for the root command and every
/// subcommand under it, keyed by the command line that prints it.
///
/// `cmd` is taken unbuilt. Building it would propagate the global options
/// onto every page and add clap's `help` subcommand, whose children mirror
/// the whole tree: the same text again under other names, none of it ours.
fn rendered_help_pages(cmd: &clap::Command) -> Vec<(String, String)> {
    let mut pages = Vec::new();
    collect_help_pages(cmd, cmd.get_name(), &mut pages);
    pages
}

fn collect_help_pages(cmd: &clap::Command, path: &str, out: &mut Vec<(String, String)>) {
    out.push((
        format!("{path} --help"),
        cmd.clone().render_long_help().to_string(),
    ));
    for sc in cmd.get_subcommands() {
        collect_help_pages(sc, &format!("{path} {}", sc.get_name()), out);
    }
}

/// The code of `content`'s production region with comments removed, line for
/// line, so a match maps back to the source line it sits on.
///
/// A line ending in a string continuation `\` runs on into the next, as the
/// literal it continues does. Every other line end is fenced with a
/// non-whitespace byte, so a phrase cannot form from the last word of one
/// statement and the first word of the next.
fn message_text(content: &str) -> String {
    let mut out = String::new();
    for line in before_test_module(content).lines() {
        let code = code_on_line(line).trim_end();
        match code.strip_suffix('\\') {
            Some(continued) => {
                out.push_str(continued);
                out.push(' ');
            }
            None => {
                out.push_str(code);
                out.push('\u{1}');
            }
        }
        out.push('\n');
    }
    out
}

/// This file, as the spelling scan reads it: its spelling table blanked and
/// its test module cut, since the table names every spelling it bans and the
/// test fixtures are seeded with the same words. Line numbers survive.
fn masked_self_text(content: &str) -> String {
    let mut out = String::new();
    let mut in_table = false;
    for line in before_test_module(content).lines() {
        if line.starts_with("const BRITISH_SPELLINGS") {
            in_table = true;
        }
        if !in_table {
            out.push_str(line);
        }
        if in_table && line.trim() == "];" {
            in_table = false;
        }
        out.push('\n');
    }
    out
}

/// The word just before byte `at` in `text`, lowercased, looking past
/// whitespace and the markdown that wraps a word (backticks, quotes,
/// emphasis).
fn preceding_word(text: &str, at: usize) -> String {
    let head = text[..at].trim_end_matches(|c: char| c.is_whitespace() || "`\"'*_".contains(c));
    head.rsplit(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or("")
        .to_lowercase()
}

/// Read `paths` into `texts` under `rules`, returning how many were read. A
/// path that does not exist is skipped: a listed page missing from disk is
/// the link check's to report.
fn push_prose_files(
    root: &Path,
    paths: impl IntoIterator<Item = PathBuf>,
    rules: ProseRules,
    texts: &mut Vec<ProseText>,
) -> usize {
    let mut read = 0;
    for path in paths {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        texts.push(ProseText {
            name: path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string(),
            text,
            rules,
        });
        read += 1;
    }
    read
}

/// Every text `check_vocabulary_and_spelling` reads, and how many it took
/// from each surface class.
///
/// **Operator surfaces, both rules**: root `README.md` and `ARCHITECTURE.md`;
/// every page `docs/SUMMARY.md` links to; the explain and prime templates and
/// the pages generated from them; the generated schemas; `help_pages`. The
/// templates are read as well as the pages, so a reintroduced term is caught
/// before regeneration copies it.
///
/// **Message text, vocabulary**: the code of every production region in
/// `src/` — comments removed, so what remains is string literals and names —
/// where `bail!`, refusal, finding and `--help` strings live. This file is
/// not shipped in `rwv` and is left out.
///
/// **Everything else, spelling**: every file `check_no_tracker_ids` reads that
/// is not an operator surface above, comments included — `src/`, `tests/`,
/// `docs/internals/`, root `CHANGELOG.md` and the shipped scripts. `CLAUDE.md`
/// is left out because it states both rules, which means naming what they
/// ban; this file is read through `masked_self_text`.
fn vocabulary_and_spelling_texts(
    root: &Path,
    help_pages: &[(String, String)],
) -> (Vec<ProseText>, Vec<(&'static str, usize)>) {
    let mut texts = Vec::new();
    let mut population = Vec::new();
    let both = ProseRules::VocabularyAndSpelling;

    let in_dir = |dir: PathBuf, ext: &'static str| {
        collect_files_in_dir(&dir, move |p| {
            p.extension().and_then(OsStr::to_str) == Some(ext)
        })
    };
    let explain_dir = root.join("docs/reference/explain");
    let prime_dir = root.join("docs/reference/prime");
    let operator_classes: [(&'static str, Vec<PathBuf>); 5] = [
        (
            "README.md and ARCHITECTURE.md",
            vec![root.join("README.md"), root.join("ARCHITECTURE.md")],
        ),
        ("summary-listed pages", collect_summary_listed_pages(root)),
        (
            "explain pages and templates",
            [
                in_dir(explain_dir.clone(), "md"),
                in_dir(explain_dir.join("templates"), "tmpl"),
            ]
            .concat(),
        ),
        (
            "prime overview and template",
            [
                in_dir(prime_dir.clone(), "md"),
                in_dir(prime_dir.join("templates"), "tmpl"),
            ]
            .concat(),
        ),
        (
            "generated schemas",
            in_dir(root.join("docs/reference/schemas"), "json"),
        ),
    ];
    let mut operator_paths: HashSet<PathBuf> = HashSet::new();
    for (class, paths) in operator_classes {
        operator_paths.extend(paths.iter().cloned());
        population.push((class, push_prose_files(root, paths, both, &mut texts)));
    }

    for (name, text) in help_pages {
        texts.push(ProseText {
            name: name.clone(),
            text: text.clone(),
            rules: both,
        });
    }
    population.push(("--help pages", help_pages.len()));

    let self_path = root.join("src/bin/generate-explain.rs");
    let mut messages = 0;
    for path in collect_rs_files(&root.join("src")) {
        if path == self_path {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        texts.push(ProseText {
            name: path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string(),
            text: message_text(&content),
            rules: ProseRules::Vocabulary,
        });
        messages += 1;
    }
    population.push(("src message text", messages));

    let mut rest = src_docs_and_tests_files(root);
    rest.extend(root_level_md_files(root));
    rest.extend(root_level_rs_files(root));
    rest.extend(root_level_toml_files(root));
    rest.extend(shipped_prose_auxiliary_files(root));
    rest.retain(|p| !operator_paths.contains(p) && *p != root.join("CLAUDE.md"));
    rest.sort();
    rest.dedup();
    let mut spelling_only = 0;
    for path in rest {
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let text = if path == self_path {
            masked_self_text(&content)
        } else {
            content
        };
        texts.push(ProseText {
            name: path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string(),
            text,
            rules: ProseRules::Spelling,
        });
        spelling_only += 1;
    }
    population.push(("other prose, spelling only", spelling_only));

    (texts, population)
}

/// Scan for the canonical-vocabulary and American-spelling rules in
/// `CLAUDE.md`: `BANNED_SYNONYMS` on operator surfaces and in message text,
/// `BRITISH_SPELLINGS` in all prose. `vocabulary_and_spelling_texts` defines
/// the scope.
///
/// A surface class that yields nothing is itself a finding, so a broken
/// collector cannot pass for a clean tree.
///
/// **What is not covered.** A phrase is matched within one text, so one split
/// across two string literals joined by `concat!` or `format!` arguments is
/// not seen, and in message text a phrase wrapped inside a multi-line literal
/// without a `\` continuation is not either. Comments in `src/` and `tests/`,
/// `docs/internals/` and `CHANGELOG.md` answer to the spelling rule only; the
/// vocabulary rule covers what an operator reads. The spelling families are
/// closed lists: a British spelling outside them passes.
fn check_vocabulary_and_spelling(root: &Path, help_pages: &[(String, String)]) -> Vec<String> {
    let compile = |terms: &'static [BannedTerm]| -> Vec<(Regex, &'static BannedTerm)> {
        terms
            .iter()
            .map(|t| {
                let re = Regex::new(&format!(r"(?i)\b(?:{})\b", t.pattern))
                    .expect("banned-term pattern compiles");
                (re, t)
            })
            .collect()
    };
    let synonyms = compile(BANNED_SYNONYMS);
    let spellings = compile(BRITISH_SPELLINGS);

    let (texts, population) = vocabulary_and_spelling_texts(root, help_pages);
    let mut findings = Vec::new();
    for (class, count) in &population {
        if *count == 0 {
            findings.push(format!(
                "vocabulary-and-spelling scan: {class} yielded nothing — the scan \
                 population must not be empty"
            ));
        }
    }
    for text in &texts {
        let mut report = |terms: &[(Regex, &'static BannedTerm)], rule: &str| {
            for (re, term) in terms {
                for m in re.find_iter(&text.text) {
                    if term
                        .unless_after
                        .contains(&preceding_word(&text.text, m.start()).as_str())
                    {
                        continue;
                    }
                    let line = text.text[..m.start()].matches('\n').count() + 1;
                    let found = m.as_str().split_whitespace().collect::<Vec<_>>().join(" ");
                    findings.push(format!(
                        "{}:{line}: `{found}` — {rule}: {}",
                        text.name, term.write
                    ));
                }
            }
        };
        if text.rules.vocabulary() {
            report(&synonyms, "canonical vocabulary");
        }
        if text.rules.spelling() {
            report(&spellings, "American spelling");
        }
    }
    findings
}

/// Kebab-cased words an operator page writes in backticks that name nothing
/// rwv defines, and are not meant to, with what each one is.
const NON_TOKEN_WORDS: &[(&str, &str)] = &[
    ("billing-service", "an example project name"),
    ("mobile-app", "an example project name"),
    ("web-app", "an example project name"),
    ("default-push", "a Mercurial path name"),
    ("git-repo", "another tool"),
    ("maturin-import-hook", "a Python package"),
    ("write-file-atomic", "an npm package"),
    (
        "hide-dotfile",
        "a misspelled settings key, shown being refused",
    ),
    ("remote-url-mismatch", "a rename a page weighs"),
    ("vcs-seam", "a gate of this repository's build"),
];

/// Verbs an operator page names to say rwv does not have them.
const ABSENT_VERBS: &[&str] = &["branch", "commit", "exec", "publish", "pull", "run"];

/// What a backticked token on an operator page may name.
///
/// The condition registers are kept apart because each is a population the
/// scan must go on reaching. `code_words` is every other kebab-cased word the
/// shipped code spells: in the production regions of `src/` outside `src/bin/`
/// and of the bundled plugins, what string literals spell and the names serde
/// derives; and clap's command, alias, flag and value names.
struct TokenVocabulary {
    finding_kinds: HashSet<String>,
    sub_kinds: HashMap<String, HashSet<String>>,
    refusals: HashSet<String>,
    failure_kinds: HashSet<String>,
    explain_verbs: HashSet<String>,
    code_words: HashSet<String>,
}

impl TokenVocabulary {
    fn is_sub_kind(&self, token: &str) -> bool {
        self.sub_kinds.values().any(|subs| subs.contains(token))
    }

    fn is_condition(&self, token: &str) -> bool {
        self.finding_kinds.contains(token)
            || self.refusals.contains(token)
            || self.failure_kinds.contains(token)
            || self.is_sub_kind(token)
    }

    fn defines(&self, token: &str) -> bool {
        self.is_condition(token) || self.code_words.contains(token)
    }
}

fn token_vocabulary(root: &Path, cli: &clap::Command) -> TokenVocabulary {
    let (finding_kinds, sub_kinds) = doctor_finding_vocabulary();
    let sync: serde_json::Value = serde_json::from_str(&schema_sync()).expect("sync schema parses");
    let failure_kinds = ["VcsErrorOutput", "SyncFailureOutput"]
        .iter()
        .flat_map(|def| schema_variant_tags(&sync["definitions"][def]))
        .collect();
    let mut code_words = src_code_kebab_words(root);
    clap_words(cli, &mut code_words);
    // Composed by `Display`, not spelled in any literal the source scan reads.
    code_words.extend(repoweave::vcs::MidOperation::all().map(|op| op.to_string()));
    TokenVocabulary {
        finding_kinds,
        sub_kinds,
        refusals: fs::read_to_string(root.join("src/refusal.rs"))
            .map(|source| refusal_tokens(&source))
            .unwrap_or_default(),
        failure_kinds,
        explain_verbs: explain::known_verbs().map(str::to_owned).collect(),
        code_words,
    }
}

/// The tags a schemars enum definition publishes: string variants by value,
/// internally tagged ones by their `kind`, externally tagged ones by their
/// single key.
fn schema_variant_tags(def: &serde_json::Value) -> Vec<String> {
    let variants: Vec<&serde_json::Value> = match def["oneOf"].as_array() {
        Some(variants) => variants.iter().collect(),
        None => vec![def],
    };
    let strings = |values: &Vec<serde_json::Value>| -> Vec<String> {
        values
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect()
    };
    let mut tags = Vec::new();
    for variant in variants {
        if let Some(values) = variant["enum"].as_array() {
            tags.extend(strings(values));
        } else if let Some(values) = variant["properties"]["kind"]["enum"].as_array() {
            tags.extend(strings(values));
        } else if let Some([key]) = variant["required"].as_array().map(Vec::as_slice) {
            tags.extend(key.as_str().map(str::to_owned));
        }
    }
    tags
}

/// Every `kind` `rwv doctor --json` can carry, violations and integration
/// issues alike, and each violation kind's `sub_kind` tags.
fn doctor_finding_vocabulary() -> (HashSet<String>, HashMap<String, HashSet<String>>) {
    let json =
        serde_json::to_value(schema_for!(DoctorJsonOutput)).expect("doctor schema serializes");
    let mut kinds: HashSet<String> = schema_variant_tags(&json["definitions"]["IssueKindOutput"])
        .into_iter()
        .collect();
    let mut sub_kinds = HashMap::new();
    for (kind, variant) in doctor_violation_variants() {
        if variant["properties"].get("sub_kind").is_some() {
            let def = resolve_ref(&json, &variant["properties"]["sub_kind"]);
            sub_kinds.insert(kind.clone(), schema_variant_tags(def).into_iter().collect());
        }
        kinds.insert(kind);
    }
    (kinds, sub_kinds)
}

/// Every token `RefusalKind` serializes to, read from its declaration: a
/// variant's name in kebab case, or the name a `#[serde(rename = "…")]` above
/// it gives. The enum offers no enumeration to read at run time.
///
/// Fieldless variants written one to a line, which is how the enum is
/// declared; a variant written any other way is not read.
fn refusal_tokens(source: &str) -> HashSet<String> {
    let Some((_, body)) = source.split_once("pub enum RefusalKind {") else {
        return HashSet::new();
    };
    let body = body.split_once("\n}").map_or(body, |(body, _)| body);
    let mut tokens = HashSet::new();
    let mut renamed = None;
    for line in body.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("#[serde(rename = \"") {
            renamed = rest.split('"').next().map(str::to_owned);
            continue;
        }
        let Some(name) = line.strip_suffix(',') else {
            continue;
        };
        if name.starts_with(|c: char| c.is_ascii_uppercase())
            && name.chars().all(|c| c.is_ascii_alphanumeric())
        {
            tokens.insert(renamed.take().unwrap_or_else(|| kebab_case(name)));
        }
    }
    tokens
}

/// serde's `rename_all = "kebab-case"`.
fn kebab_case(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.char_indices() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn kebab_word_re() -> Regex {
    Regex::new(r"\b[a-z][a-z0-9]*(?:-[a-z0-9]+)+\b").expect("kebab pattern compiles")
}

/// Kebab-cased words in the shipped production code of `src/` — outside
/// `src/bin/`, plus the bundled plugins: what its string literals spell, and
/// the names serde derives for every item declared `rename_all = "kebab-case"`.
///
/// Comments are not code and a test module is not production, so neither can
/// vouch for a word a page cites; the rest of `src/bin/` is not shipped, and
/// holds this gate's own tables.
fn src_code_kebab_words(root: &Path) -> HashSet<String> {
    let re = kebab_word_re();
    let bin = root.join("src/bin");
    let plugins: Vec<PathBuf> = PLUGIN_SOURCE_DIRS.iter().map(|d| root.join(d)).collect();
    let mut words = HashSet::new();
    for path in collect_rs_files(&root.join("src")) {
        if path.starts_with(&bin) && !plugins.iter().any(|p| path.starts_with(p)) {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let production = before_test_module(&content);
        for line in production.lines() {
            words.extend(
                re.find_iter(code_on_line(line))
                    .map(|m| m.as_str().to_owned()),
            );
        }
        words.extend(serde_kebab_names(production));
    }
    words
}

/// The names serde gives the variants or fields of each enum or struct
/// declared `rename_all = "kebab-case"` in `source`. A member with a
/// `rename` of its own is left to the literal that renames it.
///
/// Members are read one to a line, at the item's top level, from the item's
/// opening line to the closing brace at its own indentation.
fn serde_kebab_names(source: &str) -> Vec<String> {
    let lines: Vec<&str> = source.lines().collect();
    let mut names = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        if !(line.trim_start().starts_with("#[serde(")
            && line.contains("rename_all = \"kebab-case\""))
        {
            continue;
        }
        let Some((open, decl)) = lines.iter().enumerate().skip(at + 1).find(|(_, l)| {
            let t = l.trim_start();
            !(t.starts_with('#') || t.starts_with("//"))
        }) else {
            continue;
        };
        let words: Vec<&str> = decl.split_whitespace().collect();
        let is_enum = words.contains(&"enum");
        if !(is_enum || words.contains(&"struct")) || !decl.trim_end().ends_with('{') {
            continue;
        }
        let close = format!("{}}}", &decl[..decl.len() - decl.trim_start().len()]);
        let mut depth = 0usize;
        let mut renamed = false;
        for member in &lines[open + 1..] {
            if member.trim_end() == close {
                break;
            }
            let code = code_on_line(member).trim();
            if depth == 0 {
                if code.starts_with("#[serde(") && code.contains("rename = ") {
                    renamed = true;
                } else if !code.is_empty() && !code.starts_with('#') {
                    let field = code
                        .trim_start_matches("pub(crate) ")
                        .trim_start_matches("pub(super) ")
                        .trim_start_matches("pub ");
                    let ident: String = field
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect();
                    let names_member = if is_enum {
                        ident.starts_with(|c: char| c.is_ascii_uppercase())
                    } else {
                        field[ident.len()..].trim_start().starts_with(':')
                    };
                    if names_member && !std::mem::take(&mut renamed) {
                        names.push(if is_enum {
                            kebab_case(&ident)
                        } else {
                            ident.replace('_', "-")
                        });
                    }
                }
            }
            depth = (depth + code.matches('{').count()).saturating_sub(code.matches('}').count());
        }
    }
    names
}

fn clap_words(cmd: &clap::Command, out: &mut HashSet<String>) {
    out.insert(cmd.get_name().to_owned());
    out.extend(cmd.get_all_aliases().map(str::to_owned));
    for arg in cmd.get_arguments() {
        out.extend(arg.get_long().map(str::to_owned));
        out.extend(
            arg.get_all_aliases()
                .unwrap_or_default()
                .into_iter()
                .map(str::to_owned),
        );
        out.extend(
            arg.get_possible_values()
                .iter()
                .map(|v| v.get_name().to_owned()),
        );
    }
    for sc in cmd.get_subcommands() {
        clap_words(sc, out);
    }
}

/// A backticked span on a page: its text, with line ends read as spaces; the
/// line it opens on; and the prose between it and the span before it in the
/// same paragraph.
struct CodeSpan {
    line: usize,
    text: String,
    gap: String,
}

/// A page as the doc-token scan reads it: the code spans of its prose, and
/// the lines of its fenced blocks.
struct PageCode {
    spans: Vec<CodeSpan>,
    fenced: Vec<(usize, String)>,
}

fn page_code(text: &str) -> PageCode {
    let mut spans = Vec::new();
    let mut fenced = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    let mut paragraph: Vec<(usize, &str)> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        let run = |c: char| trimmed.len() - trimmed.trim_start_matches(c).len();
        if let Some((c, len)) = fence {
            if run(c) >= len && trimmed.trim_start_matches(c).trim().is_empty() {
                fence = None;
            } else {
                fenced.push((i + 1, line.to_owned()));
            }
            continue;
        }
        if let Some(c) = ['`', '~'].into_iter().find(|&c| run(c) >= 3) {
            paragraph_spans(&paragraph, &mut spans);
            paragraph.clear();
            fence = Some((c, run(c)));
        } else if line.trim().is_empty() {
            paragraph_spans(&paragraph, &mut spans);
            paragraph.clear();
        } else {
            paragraph.push((i + 1, line));
        }
    }
    paragraph_spans(&paragraph, &mut spans);
    PageCode { spans, fenced }
}

/// The code spans of one paragraph. A span closes at the next run of exactly
/// as many backticks as opened it, and cannot leave its paragraph; a run with
/// no such partner is literal text.
fn paragraph_spans(lines: &[(usize, &str)], out: &mut Vec<CodeSpan>) {
    let Some(&(first_line, _)) = lines.first() else {
        return;
    };
    let text = lines.iter().map(|(_, l)| *l).collect::<Vec<_>>().join("\n");
    let bytes = text.as_bytes();
    let backtick_run = |from: usize| bytes[from..].iter().take_while(|&&b| b == b'`').count();
    let mut at = 0;
    let mut gap_from = 0;
    while at < bytes.len() {
        if bytes[at] != b'`' {
            at += 1;
            continue;
        }
        let ticks = backtick_run(at);
        let open = at;
        at += ticks;
        let mut probe = at;
        let mut close = None;
        while probe < bytes.len() {
            if bytes[probe] == b'`' {
                let run = backtick_run(probe);
                if run == ticks {
                    close = Some(probe);
                    break;
                }
                probe += run;
            } else {
                probe += 1;
            }
        }
        let Some(close) = close else {
            continue;
        };
        out.push(CodeSpan {
            line: first_line + text[..open].matches('\n').count(),
            text: text[at..close].replace('\n', " ").trim().to_owned(),
            gap: text[gap_from..open].to_owned(),
        });
        at = close + ticks;
        gap_from = at;
    }
}

fn is_kebab_token(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.contains('-')
        && s.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

/// The spans a page writes as sub-kinds of the finding kind before them, each
/// paired with that kind: in a parenthesis opened straight after the kind, as
/// in "`kind` (`a`, `b`)", or after a slash, as in "`kind` / `a`", up to the
/// first separator that is not a comma, "and" or "or", or the first span that
/// is not shaped like a token. A link target on the kind is passed over.
fn qualified_sub_kinds<'s>(
    spans: &'s [CodeSpan],
    vocab: &TokenVocabulary,
) -> Vec<(&'s CodeSpan, &'s CodeSpan)> {
    let token_shaped = |s: &str| {
        s.starts_with(|c: char| c.is_ascii_lowercase())
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    };
    let mut out = Vec::new();
    for (i, kind) in spans.iter().enumerate() {
        if !vocab.sub_kinds.contains_key(&kind.text) {
            continue;
        }
        let Some(first) = spans.get(i + 1) else {
            continue;
        };
        let opener = match first.gap.strip_prefix("](") {
            Some(rest) => rest.split_once(')').map_or("", |(_, after)| after),
            None => first.gap.as_str(),
        };
        if !matches!(opener.trim(), "(" | "/") {
            continue;
        }
        for (j, sub) in spans.iter().enumerate().skip(i + 1) {
            let joined =
                j == i + 1 || matches!(sub.gap.trim(), "," | "and" | ", and" | "or" | ", or");
            if !joined || !token_shaped(&sub.text) {
                break;
            }
            out.push((kind, sub));
        }
    }
    out
}

/// The `rwv` invocations in a command line: every `&&`, `||` or ` | `
/// segment that starts with `rwv`, as its words after `rwv`, with a trailing
/// `# comment` dropped.
fn rwv_invocations(line: &str) -> Vec<Vec<&str>> {
    let line = line.trim_start();
    let line = line.strip_prefix("$ ").unwrap_or(line);
    let line = line.split(" # ").next().unwrap_or(line);
    Regex::new(r"&&|\|\|| \| ")
        .expect("separator pattern compiles")
        .split(line)
        .filter_map(|segment| {
            let mut words = segment.split_whitespace();
            (words.next() == Some("rwv")).then(|| words.collect())
        })
        .collect()
}

fn takes_value(chain: &[&clap::Command], word: &str) -> bool {
    if word.contains('=') {
        return false;
    }
    chain
        .iter()
        .rev()
        .flat_map(|cmd| cmd.get_arguments())
        .find(|arg| match word.strip_prefix("--") {
            Some(long) => arg.get_long() == Some(long),
            None => word.len() == 2 && arg.get_short() == word.chars().nth(1),
        })
        .is_some_and(|arg| arg.get_action().takes_values())
}

fn is_placeholder(word: &str) -> bool {
    word.starts_with(|c: char| "<[{\"'$".contains(c))
        || word.contains("...")
        || word.contains('…')
        || (word.contains(|c: char| c.is_ascii_uppercase())
            && word
                .chars()
                .all(|c| c.is_ascii_uppercase() || c == '_' || c == '-'))
}

fn is_command_word(word: &str) -> bool {
    word.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && word
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-._".contains(c))
}

/// True if `page` names the executable `rwv-<verb>`, which `rwv <verb>`
/// reaches as a plugin.
fn names_plugin(page: &str, verb: &str) -> bool {
    let name = format!("rwv-{verb}");
    page.match_indices(&name).any(|(at, _)| {
        !page[at + name.len()..]
            .starts_with(|c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    })
}

/// Where an invocation's words land in clap's tree.
enum Invocation<'c> {
    Command {
        path: Vec<&'c str>,
        positional_before_subcommand: bool,
    },
    PluginOrAbsentVerb,
    NoVerb,
}

/// Locate an invocation's words in clap's tree.
///
/// A word in a command's subcommand position must name one of its
/// subcommands — unless the command also takes a positional and a subcommand
/// follows, as in `rwv workweave web-app create`. At the root, a word that
/// names no subcommand may be a bundled plugin, a plugin the same page names
/// as an `rwv-<verb>` executable, or a verb in `ABSENT_VERBS`. A placeholder stands for a value,
/// and a leaf command's arguments are not read.
fn locate_invocation<'c>(
    cli: &'c clap::Command,
    words: &[&str],
    page: &str,
) -> Result<Invocation<'c>, String> {
    let mut chain = vec![cli];
    let mut path: Vec<&'c str> = Vec::new();
    let mut unresolved: Option<&str> = None;
    let mut positional_before_subcommand = false;
    let mut rest = words.iter();
    while let Some(&word) = rest.next() {
        let cmd = *chain.last().expect("the chain starts at the root");
        if word.starts_with('-') && word.len() > 1 {
            if takes_value(&chain, word) {
                rest.next();
            }
            continue;
        }
        if !cmd.has_subcommands() || (path.is_empty() && !is_command_word(word)) {
            break;
        }
        if is_placeholder(word) || !is_command_word(word) {
            continue;
        }
        if let Some(sub) = cmd.find_subcommand(word) {
            path.push(sub.get_name());
            chain.push(sub);
            positional_before_subcommand |= unresolved.take().is_some();
            continue;
        }
        if path.is_empty() {
            if is_bundled_plugin(word) || names_plugin(page, word) || ABSENT_VERBS.contains(&word) {
                return Ok(Invocation::PluginOrAbsentVerb);
            }
            return Err(format!(
                "`rwv {word}` is not a verb, and this page names no `rwv-{word}` plugin"
            ));
        }
        let parent = path.join(" ");
        match unresolved {
            None if cmd.get_positionals().next().is_some() => unresolved = Some(word),
            None => return Err(not_a_subcommand(&path, word)),
            Some(first) => {
                return Err(format!(
                    "`rwv {parent} {first} {word}` is not a command: neither `{first}` nor \
                     `{word}` is a subcommand of `rwv {parent}`"
                ))
            }
        }
    }
    if let Some(word) = unresolved {
        return Err(not_a_subcommand(&path, word));
    }
    Ok(if path.is_empty() {
        Invocation::NoVerb
    } else {
        Invocation::Command {
            path,
            positional_before_subcommand,
        }
    })
}

fn not_a_subcommand(path: &[&str], word: &str) -> String {
    let parent = path.join(" ");
    format!("`rwv {parent} {word}` is not a command: `{word}` is no subcommand of `rwv {parent}`")
}

/// The check an invocation's token argument faces, if it has one: what
/// `rwv explain` is asked to serve must be a verb it explains or a condition
/// token, and what `rwv doctor --kind` is asked to filter on must be a
/// finding kind. A placeholder is not read.
fn invocation_argument(
    path: &[&str],
    words: &[&str],
    vocab: &TokenVocabulary,
) -> Option<Result<(), String>> {
    let checked = |token: &str, holds: bool, message: String| {
        (!is_placeholder(token)).then_some(if holds { Ok(()) } else { Err(message) })
    };
    match path {
        ["explain"] => {
            let at = words.iter().position(|w| *w == "explain")?;
            let token = *words[at + 1..].iter().find(|w| !w.starts_with('-'))?;
            checked(
                token,
                vocab.explain_verbs.contains(token) || vocab.is_condition(token),
                format!(
                    "`rwv explain {token}` serves nothing: `{token}` is neither a verb nor a token"
                ),
            )
        }
        ["doctor"] => {
            let kind =
                words
                    .iter()
                    .enumerate()
                    .find_map(|(i, w)| match w.strip_prefix("--kind=") {
                        Some(value) => Some(value),
                        None => (*w == "--kind")
                            .then(|| words.get(i + 1).copied())
                            .flatten(),
                    })?;
            checked(
                kind,
                vocab.finding_kinds.contains(kind),
                format!("`rwv doctor --kind {kind}` refuses: `{kind}` is not a finding kind"),
            )
        }
        _ => None,
    }
}

/// How many citations of each token class, and through each recognition
/// branch, the scan read.
#[derive(Default)]
struct DocTokenPopulation {
    finding_kinds: usize,
    sub_kinds: usize,
    refusals: usize,
    failure_kinds: usize,
    verbs: usize,
    qualified_sub_kinds: usize,
    inline_invocations: usize,
    fenced_invocations: usize,
    positional_before_subcommand: usize,
    plugin_or_absent_verbs: usize,
    token_arguments: usize,
}

impl DocTokenPopulation {
    /// Each token class and recognition branch, what it yielded, and the
    /// least it may yield over this repository's operator pages before the
    /// scan is taken to have gone blind rather than clean. A branch's floor is
    /// one: it may not go dark while the classes it feeds stay full.
    fn floors(&self) -> [(&'static str, usize, usize); 11] {
        [
            ("finding kinds", self.finding_kinds, 150),
            ("sub-kinds", self.sub_kinds, 50),
            ("refusal tokens", self.refusals, 150),
            ("VCS and sync failure kinds", self.failure_kinds, 9),
            ("verbs", self.verbs, 1000),
            (
                "sub-kinds qualified by their kind",
                self.qualified_sub_kinds,
                1,
            ),
            ("invocations in prose", self.inline_invocations, 1),
            ("invocations in fenced blocks", self.fenced_invocations, 1),
            (
                "positionals before a subcommand",
                self.positional_before_subcommand,
                1,
            ),
            ("plugins and absent verbs", self.plugin_or_absent_verbs, 1),
            ("token arguments", self.token_arguments, 1),
        ]
    }
}

/// Every operator page the doc-token scan reads: each page `docs/SUMMARY.md`
/// lists, and root `README.md`.
fn doc_token_pages(root: &Path) -> Vec<PathBuf> {
    let mut pages = collect_summary_listed_pages(root);
    pages.push(root.join("README.md"));
    pages
}

/// Scan the operator pages for cited tokens that do not exist.
///
/// **What is read.** In prose, every backticked span: a kebab-cased one must
/// be a word `vocab` defines, a plugin executable's `rwv-` name, or one of
/// `NON_TOKEN_WORDS`; one written as a sub-kind of the finding kind before it
/// must be a sub-kind of that kind; and one opening with `rwv` must resolve in
/// clap's tree, as must every `rwv` line in a fenced block. Through
/// `rwv explain` and `rwv doctor --kind`, the token argument is checked too.
///
/// **What is not.** A token outside backticks, or inside a span that holds
/// more than the token; a single word, which reads the same as an English one
/// — so a sub-kind like `detached` is checked only where it is written as a
/// sub-kind of its kind; a flag, and every argument of a leaf command but the
/// two above. Existence is all it checks: a token cited for the wrong
/// condition passes, and so does any word that some production literal in
/// `src/` happens to spell.
fn scan_doc_tokens(
    root: &Path,
    vocab: &TokenVocabulary,
    cli: &clap::Command,
) -> (Vec<String>, DocTokenPopulation) {
    let mut findings = Vec::new();
    let mut population = DocTokenPopulation::default();
    for path in doc_token_pages(root) {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let name = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string();
        let code = page_code(&text);

        let qualified = qualified_sub_kinds(&code.spans, vocab);
        population.qualified_sub_kinds += qualified.len();
        for (kind, sub) in &qualified {
            if !vocab.sub_kinds[&kind.text].contains(&sub.text) {
                let mut known: Vec<&String> = vocab.sub_kinds[&kind.text].iter().collect();
                known.sort();
                findings.push(format!(
                    "{name}:{}: `{}` is not a sub-kind of `{}`, whose sub-kinds are {}",
                    sub.line,
                    sub.text,
                    kind.text,
                    known
                        .iter()
                        .map(|s| format!("`{s}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }

        let mut invocations: Vec<(usize, String, bool)> = Vec::new();
        for span in &code.spans {
            if span.text.starts_with("rwv ") {
                invocations.push((span.line, span.text.clone(), true));
                continue;
            }
            if !is_kebab_token(&span.text) {
                continue;
            }
            let token = span.text.as_str();
            let counted = [
                (
                    &mut population.finding_kinds,
                    vocab.finding_kinds.contains(token),
                ),
                (&mut population.sub_kinds, vocab.is_sub_kind(token)),
                (&mut population.refusals, vocab.refusals.contains(token)),
                (
                    &mut population.failure_kinds,
                    vocab.failure_kinds.contains(token),
                ),
            ];
            for (count, member) in counted {
                *count += usize::from(member);
            }
            let checked_as_sub_kind = qualified.iter().any(|(_, sub)| std::ptr::eq(*sub, span));
            if !(checked_as_sub_kind
                || vocab.defines(token)
                || token.starts_with("rwv-")
                || NON_TOKEN_WORDS.iter().any(|(word, _)| *word == token))
            {
                findings.push(format!(
                    "{name}:{}: `{token}` is not a token rwv defines: no finding kind, \
                     sub-kind, refusal, failure kind, command, flag, wire value or \
                     code literal is spelled this way",
                    span.line
                ));
            }
        }
        invocations.extend(
            code.fenced
                .iter()
                .map(|(line, text)| (*line, text.clone(), false)),
        );

        for (line, command, inline) in &invocations {
            for words in rwv_invocations(command) {
                if *inline {
                    population.inline_invocations += 1;
                } else {
                    population.fenced_invocations += 1;
                }
                match locate_invocation(cli, &words, &text) {
                    Ok(Invocation::NoVerb) => {}
                    Ok(Invocation::PluginOrAbsentVerb) => population.plugin_or_absent_verbs += 1,
                    Ok(Invocation::Command {
                        path,
                        positional_before_subcommand,
                    }) => {
                        population.verbs += 1;
                        population.positional_before_subcommand +=
                            usize::from(positional_before_subcommand);
                        if let Some(checked) = invocation_argument(&path, &words, vocab) {
                            population.token_arguments += 1;
                            if let Err(message) = checked {
                                findings.push(format!("{name}:{line}: {message}"));
                            }
                        }
                    }
                    Err(message) => findings.push(format!("{name}:{line}: {message}")),
                }
            }
        }
    }
    (findings, population)
}

fn floor_findings(population: &DocTokenPopulation) -> Vec<String> {
    population
        .floors()
        .into_iter()
        .filter(|(_, count, floor)| count < floor)
        .map(|(class, count, floor)| {
            format!(
                "doc-token scan: {class} yielded {count} citations, under the floor of \
                 {floor} — the scan has stopped reading them, or the pages stopped citing them"
            )
        })
        .collect()
}

/// clap's tree as parsed: global options propagated and `help` added.
fn built_cli(cli: &clap::Command) -> clap::Command {
    let mut cli = cli.clone();
    cli.build();
    cli
}

/// The doc-token gate: `scan_doc_tokens` over this repository's operator
/// pages, and a class or branch that yields fewer citations than its floor
/// reported as a finding of its own.
fn check_doc_tokens(root: &Path, cli: &clap::Command) -> Vec<String> {
    let cli = built_cli(cli);
    let vocab = token_vocabulary(root, &cli);
    let (mut findings, population) = scan_doc_tokens(root, &vocab, &cli);
    findings.extend(floor_findings(&population));
    findings
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
            "cannot read docs/SUMMARY.md at {}: {e:#}",
            summary_path.display()
        )
    })?;
    let summary_stems = collect_joints_stems_from_summary(&summary);

    // Parse docs/explanation/index.md: collect every stem under ./joints/.
    let index_path = root.join("docs/explanation/index.md");
    let index = fs::read_to_string(&index_path).map_err(|e| {
        anyhow::anyhow!(
            "cannot read docs/explanation/index.md at {}: {e:#}",
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
                "missing prime overview template at {}: {e:#}",
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
                "missing template for verb '{}' at {}: {e:#}",
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
            .map_err(|e| anyhow::anyhow!("verb '{}': {e:#}", verb.name))?;

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

        // Write the per-record NDJSON schema artifact alongside the envelope
        // artifact. Verbs that emit NDJSON records (`-j N`, `N > 1`) have a
        // `schema_record` generator; verbs without an NDJSON mode skip this.
        if let Some(gen_record) = verb.schema_record {
            let record_schema = gen_record();
            let mut content = record_schema;
            if !content.ends_with('\n') {
                content.push('\n');
            }
            let record_path = schemas_dir.join(format!("{}-record.json", verb.name));
            write_if_changed(&record_path, &content)?;
        }
    }

    // The placement line every file-placing verb streams beside its own
    // records belongs to no one verb.
    let mut placement_record = schema_placement_record();
    placement_record.push('\n');
    write_if_changed(
        &schemas_dir.join("placement-record.json"),
        &placement_record,
    )?;

    // Replay's member lines are streamed by both sync verbs; advance-target's
    // lines by sync-to.
    for (name, generate) in [
        ("member-record", schema_member_record as fn() -> String),
        ("advance-record", schema_advance_record),
    ] {
        let mut content = generate();
        content.push('\n');
        write_if_changed(&schemas_dir.join(format!("{name}.json")), &content)?;
    }

    for (name, schema) in [
        ("each", schema_for!(each_wire::EachJsonOutput)),
        (
            "each-record",
            schema_for!(each_wire::EachNdjsonRecord<'static>),
        ),
    ] {
        let mut content = serde_json::to_string_pretty(&schema).expect("each schema serializes");
        content.push('\n');
        write_if_changed(&schemas_dir.join(format!("{name}.json")), &content)?;
    }

    // Sanity-check schema URLs the binary embeds match the artifact paths we
    // just wrote. If a URL drifts (e.g. someone changes the crate version
    // path), this surfaces immediately.
    assert!(
        SYNC_JSON_SCHEMA_URL.ends_with("/docs/reference/schemas/sync.json"),
        "SYNC_JSON_SCHEMA_URL no longer points at the committed artifact"
    );
    assert!(
        SYNC_RECORD_SCHEMA_URL.ends_with("/docs/reference/schemas/sync-record.json"),
        "SYNC_RECORD_SCHEMA_URL no longer points at the committed artifact"
    );
    assert!(
        SYNC_TO_JSON_SCHEMA_URL.ends_with("/docs/reference/schemas/sync-to.json"),
        "SYNC_TO_JSON_SCHEMA_URL no longer points at the committed artifact"
    );
    assert!(
        SYNC_TO_RECORD_SCHEMA_URL.ends_with("/docs/reference/schemas/sync-to-record.json"),
        "SYNC_TO_RECORD_SCHEMA_URL no longer points at the committed artifact"
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
        FETCH_RECORD_SCHEMA_URL.ends_with("/docs/reference/schemas/fetch-record.json"),
        "FETCH_RECORD_SCHEMA_URL no longer points at the committed artifact"
    );
    assert!(
        UPDATE_SCHEMA_URL.ends_with("/docs/reference/schemas/update.json"),
        "UPDATE_SCHEMA_URL no longer points at the committed artifact"
    );
    assert!(
        UPDATE_RECORD_SCHEMA_URL.ends_with("/docs/reference/schemas/update-record.json"),
        "UPDATE_RECORD_SCHEMA_URL no longer points at the committed artifact"
    );
    assert!(
        PUSH_SCHEMA_URL.ends_with("/docs/reference/schemas/push.json"),
        "PUSH_SCHEMA_URL no longer points at the committed artifact"
    );
    assert!(
        PUSH_RECORD_SCHEMA_URL.ends_with("/docs/reference/schemas/push-record.json"),
        "PUSH_RECORD_SCHEMA_URL no longer points at the committed artifact"
    );
    assert!(
        PLACEMENT_RECORD_SCHEMA_URL.ends_with("/docs/reference/schemas/placement-record.json"),
        "PLACEMENT_RECORD_SCHEMA_URL no longer points at the committed artifact"
    );
    assert!(
        MEMBER_RECORD_SCHEMA_URL.ends_with("/docs/reference/schemas/member-record.json"),
        "MEMBER_RECORD_SCHEMA_URL no longer points at the committed artifact"
    );
    assert!(
        ADVANCE_RECORD_SCHEMA_URL.ends_with("/docs/reference/schemas/advance-record.json"),
        "ADVANCE_RECORD_SCHEMA_URL no longer points at the committed artifact"
    );
    let schema_base = repoweave::fetch::FETCH_SCHEMA_URL
        .strip_suffix("fetch.json")
        .expect("FETCH_SCHEMA_URL ends with fetch.json");
    assert_eq!(
        each_schema_url::EACH_SCHEMA_URL,
        format!("{schema_base}each.json"),
        "rwv-each's EACH_SCHEMA_URL no longer follows the schema-URL template"
    );
    assert_eq!(
        each_schema_url::EACH_RECORD_SCHEMA_URL,
        format!("{schema_base}each-record.json"),
        "rwv-each's EACH_RECORD_SCHEMA_URL no longer follows the schema-URL template"
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

    // --- help-flag-ordering gate ------------------------------------------
    // No assembled explain page may tell the reader to run
    // `rwv --help <verb>`: clap resolves --help before the subcommand, so
    // that spelling prints the top-level help, not the verb's.
    let help_flag_errors = check_help_flag_ordering(&docs_dir);
    if !help_flag_errors.is_empty() {
        let msg = help_flag_errors.join("\n");
        anyhow::bail!(
            "docs failed help-flag-ordering check:\n{msg}\n\n\
             Fix: write `rwv <verb> --help`, not `rwv --help <verb>`."
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

    // --- plugin-boundary gate ------------------------------------------------
    // The bundled plugin shares this package, so nothing but this check stops
    // its source from linking the library a third-party plugin cannot.
    let boundary_errors = PLUGIN_SOURCE_DIRS
        .iter()
        .flat_map(|dir| check_plugin_library_boundary(&root.join(dir), LIBRARY_CRATE))
        .collect::<Vec<_>>();
    if !boundary_errors.is_empty() {
        let msg = boundary_errors.join("\n");
        anyhow::bail!(
            "plugin-boundary check failed:\n{msg}\n\n\
             Fix: a bundled plugin reaches rwv the way any plugin does — the \
             `RWV_*` envelope, argv, and `--json` output — never through the \
             library."
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
             Fix: describe the behavior in rwv's own terms."
        );
    }

    // --- operator-surface internals gate ---------------------------------
    // The generator is an audience boundary: comment text landing on
    // docs/reference/explain/** or docs/reference/schemas/*.json, or prose
    // on any page docs/SUMMARY.md lists, must not point at docs/internals/,
    // which mdBook does not render.
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

    // --- vocabulary-and-spelling gate ------------------------------------
    // Operator surfaces and message text use the canonical vocabulary, and
    // all prose uses American spelling. --help is rendered from the clap
    // tree, so every verb's help is read the way an operator sees it.
    let prose_errors = check_vocabulary_and_spelling(&root, &rendered_help_pages(&Cli::command()));
    if !prose_errors.is_empty() {
        let msg = prose_errors.join("\n");
        anyhow::bail!(
            "vocabulary-and-spelling check failed:\n{msg}\n\n\
             Fix: write the term the finding names. The vocabulary and spelling \
             rules are in CLAUDE.md, under \"Canonical vocabulary\" and \
             \"American spelling\"."
        );
    }

    // --- doc-token gate ----------------------------------------------------
    // A finding kind, sub-kind, refusal token or verb an operator page cites
    // must exist in the code, or the page sends its reader to a name no
    // command prints and `rwv explain` cannot serve.
    let doc_token_errors = check_doc_tokens(&root, &Cli::command());
    if !doc_token_errors.is_empty() {
        let msg = doc_token_errors.join("\n");
        anyhow::bail!(
            "doc-token check failed:\n{msg}\n\n\
             Fix: cite the token the code defines — `rwv explain <token>` serves \
             every condition token, and `rwv <verb> --help` lists every \
             subcommand. A word not meant as a token goes in NON_TOKEN_WORDS, \
             and a verb a page names to say rwv lacks it goes in ABSENT_VERBS, \
             both in src/bin/generate-explain.rs."
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
    /// This binary is its own crate, so it cannot reach the library's
    /// `#[cfg(test)]` `report_skip` — a `cfg(test)` item does not exist for a
    /// dependent. Same bytes for the same reason: libtest intercepts the print
    /// macros and discards what a PASSING test wrote, so a skip announced with
    /// `eprintln!` reaches nobody.
    fn report_skip(reason: &str) {
        use std::io::Write;
        let _ = std::io::stderr().write_all(format!("SKIP: {reason}\n").as_bytes());
    }

    /// Findings spell paths as the platform walked them; the seeded
    /// assertions below name locations with `/`.
    fn slashed(s: &str) -> String {
        s.replace('\\', "/")
    }

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

    /// The fixture `make_tree` builds is well-formed and produces no findings.
    #[test]
    fn a_well_formed_fixture_tree_produces_no_findings() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let docs = make_tree(tmp.path());
        let errors = check_assembled_docs(&docs);
        assert!(
            errors.is_empty(),
            "expected no errors in the well-formed fixture tree, got:\n{}",
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

    // ── help-flag-ordering check unit tests ────────────────────────────────────

    /// The fixture `make_tree` builds (`rwv <verb> --help` nowhere in it)
    /// produces no findings.
    #[test]
    fn help_flag_ordering_passes_on_well_formed_fixture_tree() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let docs = make_tree(tmp.path());
        let errors = check_help_flag_ordering(&docs);
        assert!(
            errors.is_empty(),
            "expected no errors in the well-formed fixture tree, got:\n{}",
            errors.join("\n")
        );
    }

    /// Seeded-failure proof: `rwv --help <verb>` in an assembled explain page
    /// is reported, naming the file, line, and the offending text — this is
    /// the exact line `rwv-hzs1.6` reverts to watch the check go red.
    #[test]
    fn rwv_help_verb_ordering_in_explain_page_is_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let docs = make_tree(tmp.path());
        fs::write(
            docs.join("reference/explain/verb.md"),
            "# verb\n\nRun `rwv --help verb` for the full clap surface.\n",
        )
        .unwrap();
        let errors = check_help_flag_ordering(&docs);
        assert!(
            !errors.is_empty(),
            "expected an error for `rwv --help verb`, got none"
        );
        let combined = errors.join("\n");
        assert!(
            combined.contains("rwv --help verb"),
            "error should quote the offending spelling, got:\n{combined}"
        );
        assert!(
            combined.contains("verb.md:3"),
            "error should name the file and line, got:\n{combined}"
        );
    }

    /// The fixed spelling, `rwv <verb> --help`, is not flagged.
    #[test]
    fn verb_before_help_flag_is_not_flagged() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let docs = make_tree(tmp.path());
        fs::write(
            docs.join("reference/explain/verb.md"),
            // rwv-advice: not-an-invocation
            "# verb\n\nRun `rwv verb --help` for the full clap surface.\n",
        )
        .unwrap();
        let errors = check_help_flag_ordering(&docs);
        assert!(
            errors.is_empty(),
            "the verb-before-flag spelling must not be flagged, got:\n{}",
            errors.join("\n")
        );
    }

    /// `rwv --help <verb>` inside a template is not flagged — templates are
    /// excluded because the generator copies the text through unchanged, and
    /// checking the assembled output they produce is equivalent.
    #[test]
    fn rwv_help_verb_ordering_in_template_is_excluded() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let docs = make_tree(tmp.path());
        fs::write(
            docs.join("reference/explain/templates/verb.md.tmpl"),
            "{{SCHEMA}}\nRun `rwv --help verb` for the full clap surface.\n",
        )
        .unwrap();
        let errors = check_help_flag_ordering(&docs);
        assert!(
            errors.is_empty(),
            "template files must be excluded, got:\n{}",
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
                schema_record: None,
            },
            Verb {
                name: "status",
                summary: "show status",
                schema: None,
                schema_record: None,
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
            schema_record: None,
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
                schema_record: None,
            },
            Verb {
                name: "status",
                summary: "show status",
                schema: None,
                schema_record: None,
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
                schema_record: None,
            },
            Verb {
                name: "status",
                summary: "show status",
                schema: None,
                schema_record: None,
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
                .any(|e| slashed(e).contains("consumer vocabulary `tl`")),
            "expected a `tl` hit, got:\n{}",
            errors.join("\n")
        );
        assert!(
            errors
                .iter()
                .any(|e| slashed(e).contains("consumer vocabulary `epic`")),
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
                .any(|e| slashed(e).contains("tests/some_test.rs")
                    && slashed(e).contains("consumer vocabulary `epic`")),
            "expected an `epic` hit under tests/, got:\n{}",
            errors.join("\n")
        );
        assert!(
            errors
                .iter()
                .any(|e| slashed(e).contains("tests/some_test.rs")
                    && slashed(e).contains("consumer vocabulary `bead`")),
            "expected a `bead` hit under tests/, got:\n{}",
            errors.join("\n")
        );
    }

    // ── consumer-vocabulary widened-scope tests ───────────────────────────────
    //
    // Each of the three newly-scanned roots (scripts/, .githooks/,
    // .github/workflows/) gets its own seeded-failure pin.  The reverted-fix
    // probe is documented inside `consumer_vocabulary_check_reaches_scripts_dir`:
    //   revert line 12 of scripts/ci-local.sh from
    //     "# re-gating an integration tip between landings wants --stages=drift alone,"
    //   to
    //     "# a sling target re-gating an integration tip between landings wants"
    //   (any standalone consumer word in that position — "sling" is used here
    //   because "choreograph" only appeared embedded in "Choreographer" which
    //   does NOT match at word boundaries).
    //   That restores a standalone consumer word in scripts/ci-local.sh; the
    //   test below reddens.  Restore and verify clean tree before committing.

    /// A consumer word planted in `scripts/` is reported.
    ///
    /// This is the execution-coverage pin for the `scripts/` root widening.
    /// The actual violation this gate closed was in `scripts/ci-local.sh`.
    #[test]
    fn consumer_vocabulary_check_reaches_scripts_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let scripts = tmp.path().join("scripts");
        fs::create_dir_all(&scripts).unwrap();
        fs::write(
            scripts.join("ci-local.sh"),
            "#!/usr/bin/env bash\n# run with sling to dispatch.\nset -e\n",
        )
        .unwrap();
        let errors = check_no_consumer_vocabulary(tmp.path());
        assert!(
            errors
                .iter()
                .any(|e| slashed(e).contains("scripts/ci-local.sh")
                    && slashed(e).contains("consumer vocabulary `sling`")),
            "expected a `sling` hit under scripts/, got:\n{}",
            errors.join("\n")
        );
    }

    /// A consumer word planted in `.githooks/` is reported.
    ///
    /// Hooks are prose-bearing shipped artifacts; a contributor reading this
    /// file has no referent for consumer words.
    #[test]
    fn consumer_vocabulary_check_reaches_githooks_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let hooks = tmp.path().join(".githooks");
        fs::create_dir_all(&hooks).unwrap();
        fs::write(
            hooks.join("pre-push"),
            "#!/bin/sh\n# dispatched to the sling worker.\nset -e\n",
        )
        .unwrap();
        let errors = check_no_consumer_vocabulary(tmp.path());
        assert!(
            errors
                .iter()
                .any(|e| slashed(e).contains(".githooks/pre-push")
                    && slashed(e).contains("consumer vocabulary `sling`")),
            "expected a `sling` hit under .githooks/, got:\n{}",
            errors.join("\n")
        );
    }

    /// A consumer word planted in `.github/workflows/` is reported.
    ///
    /// Workflow YAML files are prose-bearing shipped artifacts; `name:` and
    /// `run:` fields are operator-facing text.
    #[test]
    fn consumer_vocabulary_check_reaches_github_workflows_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workflows = tmp.path().join(".github/workflows");
        fs::create_dir_all(&workflows).unwrap();
        fs::write(
            workflows.join("ci.yml"),
            "name: CI\non: push\njobs:\n  build:\n    # bead gate\n    runs-on: ubuntu-latest\n",
        )
        .unwrap();
        let errors = check_no_consumer_vocabulary(tmp.path());
        assert!(
            errors
                .iter()
                .any(|e| slashed(e).contains(".github/workflows/ci.yml")
                    && slashed(e).contains("consumer vocabulary `bead`")),
            "expected a `bead` hit under .github/workflows/, got:\n{}",
            errors.join("\n")
        );
    }

    /// `scripts/reap-orphaned-savepoints.sh` — recently landed, deployment-neutral
    /// — must pass the widened scan with no consumer or tracker-ID findings.
    ///
    /// This is the known-clean verification the bead called for.
    #[test]
    fn reap_orphaned_savepoints_sh_is_clean() {
        let root = repo_root();
        let script = root.join("scripts/reap-orphaned-savepoints.sh");
        if !script.exists() {
            report_skip("scripts/reap-orphaned-savepoints.sh is not in this checkout");
            return;
        }
        let consumer_errors = check_no_consumer_vocabulary(&root);
        let filtered_consumer: Vec<_> = consumer_errors
            .iter()
            .filter(|e| slashed(e).contains("reap-orphaned-savepoints.sh"))
            .collect();
        assert!(
            filtered_consumer.is_empty(),
            "reap-orphaned-savepoints.sh must be free of consumer vocabulary, got:\n{}",
            filtered_consumer
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );

        let tracker_errors = check_no_tracker_ids(&root);
        let filtered_tracker: Vec<_> = tracker_errors
            .iter()
            .filter(|e| slashed(e).contains("reap-orphaned-savepoints.sh"))
            .collect();
        assert!(
            filtered_tracker.is_empty(),
            "reap-orphaned-savepoints.sh must be free of tracker IDs, got:\n{}",
            filtered_tracker
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n")
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
            errors.iter().any(|e| slashed(e).contains("contains `rig`")),
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
            errors
                .iter()
                .any(|e| slashed(e).contains("contains `gas city`")),
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

    /// A foreign vocabulary word planted in `scripts/` is reported.
    ///
    /// The foreign-vocabulary gate is widened to cover the same auxiliary files
    /// as the consumer-vocabulary and tracker-ID gates; this pin proves the
    /// `scripts/` root is in scope.
    #[test]
    fn foreign_vocabulary_check_reaches_scripts_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let scripts = tmp.path().join("scripts");
        fs::create_dir_all(&scripts).unwrap();
        fs::write(
            scripts.join("deploy.sh"),
            "#!/usr/bin/env bash\n# push through the rig.\nset -e\n",
        )
        .unwrap();
        let errors = check_no_foreign_vocabulary(tmp.path());
        assert!(
            errors
                .iter()
                .any(|e| slashed(e).contains("scripts/deploy.sh")
                    && slashed(e).contains("contains `rig`")),
            "expected a `rig` hit under scripts/, got:\n{}",
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
            errors.iter().any(|e| slashed(e).contains(&planted_id)),
            "expected a tracker-ID hit, got:\n{}",
            errors.join("\n")
        );
    }

    /// Review-record shorthand is reported; the ordinary word is not.
    #[test]
    fn tracker_id_check_fails_on_review_shorthand() {
        let planted = format!("{} {}", "Correction", "4");
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("lib.rs"),
            format!(
                "// Atomic form ({planted}).\n// A correction to the lock; correction: none.\npub fn f() {{}}\n"
            ),
        )
        .unwrap();
        let errors = check_no_tracker_ids(tmp.path());
        assert_eq!(errors.len(), 1, "got:\n{}", errors.join("\n"));
        assert!(errors[0].contains(&planted), "got:\n{}", errors[0]);
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
                .any(|e| slashed(e).contains("tests/some_test.rs")
                    && slashed(e).contains(&planted_id)),
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
                .any(|e| slashed(e).contains("CLAUDE.md") && slashed(e).contains(&planted_id)),
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
                .any(|e| slashed(e).contains("build.rs") && slashed(e).contains(&planted_id)),
            "expected a tracker-ID hit in root-level build.rs, got:\n{}",
            errors.join("\n")
        );
    }

    /// A root-level `.toml` file — `Cargo.toml`, where a dependency comment
    /// is exactly the kind of prose a tracker ID gets written into — carries
    /// no exemption either.
    ///
    /// Same assembled-ID reason as `tracker_id_check_fails_on_fo_prefixed_id`.
    #[test]
    fn tracker_id_check_reaches_root_level_toml() {
        let planted_id = format!("{}-{}", "fo", "ab12cd.3");
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("Cargo.toml"),
            format!("# Added for {planted_id}.\nserde = \"1\"\n"),
        )
        .unwrap();
        let errors = check_no_tracker_ids(tmp.path());
        assert!(
            errors
                .iter()
                .any(|e| slashed(e).contains("Cargo.toml") && slashed(e).contains(&planted_id)),
            "expected a tracker-ID hit in root-level Cargo.toml, got:\n{}",
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
                .any(|e| slashed(e).contains("docs/guide.md") && slashed(e).contains(&planted_id)),
            "expected a tracker-ID hit under docs/, got:\n{}",
            errors.join("\n")
        );
    }

    /// A tracker ID planted in `scripts/` is reported.
    ///
    /// The tracker-ID gate is widened to cover the same auxiliary files as the
    /// consumer-vocabulary and foreign-vocabulary gates; this pin proves the
    /// `scripts/` root is in scope.
    ///
    /// Same assembled-ID reason as `tracker_id_check_fails_on_fo_prefixed_id`.
    #[test]
    fn tracker_id_check_reaches_scripts_dir() {
        let planted_id = format!("{}-{}", "fo", "ab12cd.3");
        let tmp = tempfile::tempdir().expect("tempdir");
        let scripts = tmp.path().join("scripts");
        fs::create_dir_all(&scripts).unwrap();
        fs::write(
            scripts.join("ci-local.sh"),
            format!("#!/usr/bin/env bash\n# Added for {planted_id}.\nset -e\n"),
        )
        .unwrap();
        let errors = check_no_tracker_ids(tmp.path());
        assert!(
            errors
                .iter()
                .any(|e| slashed(e).contains("scripts/ci-local.sh")
                    && slashed(e).contains(&planted_id)),
            "expected a tracker-ID hit under scripts/, got:\n{}",
            errors.join("\n")
        );
    }

    /// A tracker ID planted in `.github/workflows/` is reported.
    ///
    /// Same assembled-ID reason as `tracker_id_check_fails_on_fo_prefixed_id`.
    #[test]
    fn tracker_id_check_reaches_github_workflows_dir() {
        let planted_id = format!("{}-{}", "fo", "ab12cd.3");
        let tmp = tempfile::tempdir().expect("tempdir");
        let workflows = tmp.path().join(".github/workflows");
        fs::create_dir_all(&workflows).unwrap();
        fs::write(
            workflows.join("ci.yml"),
            format!("name: CI\n# Fixed per {planted_id}.\non: push\n"),
        )
        .unwrap();
        let errors = check_no_tracker_ids(tmp.path());
        assert!(
            errors
                .iter()
                .any(|e| slashed(e).contains(".github/workflows/ci.yml")
                    && slashed(e).contains(&planted_id)),
            "expected a tracker-ID hit under .github/workflows/, got:\n{}",
            errors.join("\n")
        );
    }

    /// A module or function name spelling the retired prefix with an
    /// underscore is reported — the shape a Rust identifier is forced into,
    /// because it cannot carry the hyphen.
    ///
    /// Two plants, because the two sites differ in what follows the slug: a
    /// numeric sub-ID, which belongs to the ID and is reported with it, and an
    /// English word, which does not and is not.
    ///
    /// Same assembled-ID reason as `tracker_id_check_fails_on_fo_prefixed_id`.
    #[test]
    fn tracker_id_check_fails_on_identifier_spelled_id() {
        let module_id = format!("{}_{}_3", "fo", "ab12cd");
        let fn_id = format!("{}_{}", "fo", "ef34gh");
        let tmp = tempfile::tempdir().expect("tempdir");
        let tests_dir = tmp.path().join("tests");
        fs::create_dir_all(&tests_dir).unwrap();
        fs::write(
            tests_dir.join("some_test.rs"),
            format!("mod {module_id} {{}}\nfn {fn_id}_regression_survives() {{}}\n"),
        )
        .unwrap();
        let errors = check_no_tracker_ids(tmp.path());
        assert!(
            errors
                .iter()
                .any(|e| slashed(e).contains(&format!("`{module_id}`"))),
            "expected the module name's ID reported with its `_3` sub-ID, got:\n{}",
            errors.join("\n")
        );
        assert!(
            errors
                .iter()
                .any(|e| slashed(e).contains(&format!("`{fn_id}`"))),
            "expected the test name's ID reported without the words after it, got:\n{}",
            errors.join("\n")
        );
    }

    /// The retired doc-claim prefix is reported, down to a three-character
    /// slug — the length the tracker-ID matcher's four-character floor would
    /// pass over, and the length a third of the retired doc-claim IDs in this
    /// repository's own history carried.
    ///
    /// Same assembled-ID reason as `tracker_id_check_fails_on_fo_prefixed_id`.
    #[test]
    fn tracker_id_check_fails_on_retired_doc_claim_id() {
        let short = format!("project-{}-{}", "reporoot", "201");
        let long = format!("project-{}-{}", "reporoot", "85h9");
        let tmp = tempfile::tempdir().expect("tempdir");
        let tests_dir = tmp.path().join("tests");
        fs::create_dir_all(&tests_dir).unwrap();
        fs::write(
            tests_dir.join("doc_claims_test.rs"),
            format!("//! Keyed to {short}\n// 2. check_missing_role ({long})\n"),
        )
        .unwrap();
        let errors = check_no_tracker_ids(tmp.path());
        for planted in [&short, &long] {
            assert!(
                errors.iter().any(|e| slashed(e).contains(planted)),
                "expected a hit for {planted}, got:\n{}",
                errors.join("\n")
            );
        }
    }

    /// The matcher holds the three retired spellings and nothing else.
    ///
    /// The retired tracker prefix is matched as prose writes it and as an
    /// identifier writes it, and the retired doc-claim prefix is matched down
    /// to a one-character slug. Everything else is out: rwv's own
    /// `rwv-`-prefixed vocabulary is the prefix the tracker issues now and
    /// stays human-caught, an embedded `fo-` is not a word, a slug outside
    /// four-to-eight lowercase-or-digit characters is not a tracker slug, and
    /// an underscore before the prefix continues an identifier rather than
    /// ending a word — without that last rule this gate reports its own test
    /// names.
    ///
    /// The planted controls carry the whole weight of this test: asserting
    /// only that the negatives stay silent would pass equally if the gate
    /// stopped reporting anything at all. Widening the matcher to a fourth
    /// spelling without amending that clause reddens the count.
    #[test]
    fn tracker_id_check_holds_the_retired_spellings_and_nothing_else() {
        let hyphen_control = format!("{}-{}", "fo", "ab12cd");
        let underscore_control = format!("{}_{}", "fo", "ef34gh");
        let claim_control = format!("project-{}-{}", "reporoot", "c3ad");
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
                 // Nor is tracker_id_check_holds_the_fo_prefix_and_more, whose\n\
                 // underscore continues an identifier rather than ending a word.\n\
                 // The three that are, one per line so each is its own finding:\n\
                 // {hyphen_control}\n\
                 // {underscore_control}\n\
                 // {claim_control}\n\
                 pub fn f() {{}}\n"
            ),
        )
        .unwrap();
        let errors = check_no_tracker_ids(tmp.path());
        assert_eq!(
            errors.len(),
            3,
            "only the three planted controls may be reported, got:\n{}",
            errors.join("\n")
        );
        for planted in [&hyphen_control, &underscore_control, &claim_control] {
            assert!(
                errors.iter().any(|e| e.contains(planted)),
                "the planted control {planted} must be reported, got:\n{}",
                errors.join("\n")
            );
        }
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

    /// **Pinning test for `tests/`'s place in the citation gate: in scope for
    /// a real citation, silent for a fixture path.**
    ///
    /// `tests/` used to be exempt outright. It no longer is — the gate now
    /// reads it — but the reason it was exempt (a test describing the
    /// fixture tree it builds in a temp directory is not citing a document)
    /// is still true for that one shape, so the widened gate has to keep
    /// being silent about it. This runs the gate end to end rather than
    /// inspecting the collectors it happens to call, so rewiring any of them
    /// fails here.
    ///
    /// Three plants, one control:
    ///   - a genuine unresolvable citation under `src/` — the control. A gate
    ///     that reported nothing at all would satisfy every assertion below
    ///     for the wrong reason.
    ///   - the same citation under `tests/` — must now be reported; this is
    ///     the boundary this change moves.
    ///   - a fixture path under `tests/`, cited in a comment AND written by
    ///     the fixture's own code in the same file — must stay silent; this
    ///     is the exemption the gate would be WRONG to drop.
    #[test]
    fn the_citation_gate_now_covers_tests_but_stays_silent_on_fixture_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("tests")).unwrap();

        let src_cites = "// The tier-0 invariants are recorded in clone-topology.md.\n\
                          pub fn f() {}\n";
        fs::write(root.join("src/lib.rs"), src_cites).unwrap();

        let tests_content = "\
            // The tier-0 invariants are recorded in clone-topology.md.\n\
            //\n\
            // Both workweaves edit notes/shared.md with conflicting content.\n\
            fn fixture(dir: &std::path::Path) {\n\
                std::fs::write(dir.join(\"notes/shared.md\"), \"x\").unwrap();\n\
            }\n";
        fs::write(root.join("tests/some_test.rs"), tests_content).unwrap();

        let findings = run_doc_citation_check(root);

        assert!(
            findings
                .iter()
                .any(|f| slashed(f).starts_with("src/lib.rs:")),
            "the gate reported nothing under src/, so its silence elsewhere \
             says nothing:\n{}",
            findings.join("\n")
        );

        let real_citation_under_tests: Vec<_> = findings
            .iter()
            .filter(|f| slashed(f).starts_with("tests/") && f.contains("clone-topology.md"))
            .collect();
        assert!(
            !real_citation_under_tests.is_empty(),
            "`tests/` is in scope now — a genuine unresolvable citation there \
             must be reported:\n{}",
            findings.join("\n")
        );

        let fixture_path_leaked: Vec<_> = findings
            .iter()
            .filter(|f| f.contains("shared.md"))
            .collect();
        assert!(
            fixture_path_leaked.is_empty(),
            "a comment naming a path the fixture itself writes must not be \
             reported — the gate would be WRONG about it:\n{fixture_path_leaked:#?}"
        );

        assert!(
            !collect_rs_files(&repo_root().join("tests")).is_empty(),
            "`tests/` holds no .rs files — the assertions above exempt nothing"
        );
    }

    /// **Pinning test for the fixture exemption's scope.** A document filename
    /// a test file's own code names exempts citations in that file and nowhere
    /// else.
    ///
    /// Both halves are asserted, because either one alone passes under a
    /// mechanism that is simply broken: a scan-wide set silences the citation
    /// next door, and no set at all reports the one the fixture stands behind.
    #[test]
    fn a_filename_one_test_files_code_names_does_not_exempt_a_citation_in_another() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("tests")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn f() {}\n").unwrap();

        fs::write(
            root.join("tests/naming_test.rs"),
            "// The refusal arms are the ones branch-model.md sets out.\n\
             fn justification() -> &'static str {\n\
             \x20   \"the ordering rule docs/internals/branch-model.md states\"\n\
             }\n",
        )
        .unwrap();
        fs::write(
            root.join("tests/citing_test.rs"),
            "// The refusal arms are the ones branch-model.md sets out.\n\
             fn t() {}\n",
        )
        .unwrap();

        let findings = run_doc_citation_check(root);
        let reported: Vec<_> = findings
            .iter()
            .filter(|f| f.contains("branch-model.md"))
            .map(|f| slashed(f))
            .collect();
        assert!(
            reported
                .iter()
                .any(|f| f.starts_with("tests/citing_test.rs")),
            "a filename named only in another file's code must not exempt this \
             citation, got:\n{}",
            findings.join("\n")
        );
        assert!(
            !reported
                .iter()
                .any(|f| f.starts_with("tests/naming_test.rs")),
            "the file whose own code names the document keeps its exemption, \
             got:\n{}",
            findings.join("\n")
        );
    }

    // ── envelope-output coverage check unit tests ─────────────────────────────

    /// A plugin-protocol page that documents all envelope vars passes the check.
    #[test]
    fn envelope_output_check_passes_when_all_documented() {
        let protocol_md = "| `RWV_VERSION` | rwv semver | never |\n\
                      | `RWV_WORKSPACE` | primary weave root | no workspace resolved |\n\
                      | `RWV_WORKWEAVE` | workweave identity | not in a workweave |\n\
                      | `RWV_WORKWEAVE_UNREGISTERED` | `1` | anywhere else |\n\
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
                      | `RWV_WORKSPACE` | primary weave root | no workspace resolved |\n\
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
            above
                .iter()
                .any(|e| slashed(e).contains("regenerable-regions.md")),
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
    /// clause is still not mechanized *as such*: when the document is named by
    /// a path that resolves, the section pointer hanging off it is invisible
    /// here. The clause above narrowed this gap; it did not close it, and a
    /// green gate must not be read as "this tree has no section pointers".
    #[test]
    fn a_section_pointer_against_a_resolving_path_is_not_mechanized() {
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

    /// **Pinning test for the line-wrap evasion.** A path broken across two
    /// comment lines is one citation to the reader, and the finding names the
    /// whole path rather than the fragment that happens to end in an
    /// extension.
    ///
    /// The shape is the one this repository actually carried, wrapped after a
    /// `/`. Reading a line at a time saw only the tail, so the report named a
    /// filename nobody wrote and pointed at the wrong line.
    #[test]
    fn a_citation_wrapped_after_a_slash_is_reported_whole_at_the_line_it_starts_on() {
        let errors = citation_errors(
            "// The abort journal's ordering is the one recorded in\n\
             // projects/foundations/docs/repoweave/sync-state-space/\n\
             // abort-intent-journal.md, and a resume replays it.\n\
             pub fn f() {}\n",
        );
        let combined = errors.join("\n");
        assert!(
            combined.contains(
                "`projects/foundations/docs/repoweave/sync-state-space/abort-intent-journal.md`"
            ),
            "the finding must name the whole wrapped path, got:\n{combined}"
        );
        assert!(
            combined.contains("lib.rs:2:"),
            "the finding belongs to the line the citation starts on, got:\n{combined}"
        );
        assert!(
            !combined.contains("lib.rs:3:"),
            "the tail line carries no citation of its own, got:\n{combined}"
        );
    }

    /// The same evasion where the break falls immediately before the
    /// extension. Neither fragment is a document token on its own — the head
    /// has no `.`, the tail has no stem — so this shape reported nothing at
    /// all, rather than reporting the wrong thing.
    #[test]
    fn a_citation_wrapped_before_its_extension_is_reported() {
        let errors = citation_errors(
            "// The refusal arms are the ones set out in docs/internals/branch-model\n\
             // .md, and the third is the one this handles.\n\
             pub fn f() {}\n",
        );
        let combined = errors.join("\n");
        assert!(
            combined.contains("`docs/internals/branch-model.md`"),
            "a break before the extension hides both fragments, got:\n{combined}"
        );
    }

    /// A wrapped path that **resolves** is followable — the reader reassembles
    /// it — so it is silent, and it satisfies a bare filename later in the
    /// same block exactly as an unwrapped path does.
    ///
    /// Reading a line at a time reported both: the tail as an unresolvable
    /// bare filename, and the bare citation the path was standing behind. A
    /// gate that reports a citation the reader can follow is wrong about it.
    #[test]
    fn a_wrapped_path_that_resolves_is_silent_and_still_satisfies_a_bare_filename() {
        let errors = citation_errors(
            "// The bottom tier of the stability stack is docs/explanation/joints/\n\
             // clone-topology.md, and detached HEAD breaks the ref-namespace\n\
             // invariant clone-topology.md records.\n\
             pub fn f() {}\n",
        );
        assert!(
            errors.is_empty(),
            "a wrapped path the reader can follow must not be reported, got:\n{}",
            errors.join("\n")
        );
    }

    /// **Pinning test for the join's guard.** Closing a break needs a path
    /// prefix on the head side. An ordinary sentence wrapping onto a filename
    /// must not fuse into a token nobody wrote — that would be the gate
    /// reporting its own invention, the one failure mode worse than a miss.
    #[test]
    fn an_ordinary_wrapped_sentence_does_not_fuse_into_a_citation() {
        let errors = citation_errors_with(
            &[("src/notes.md", "# notes\n")],
            "// Every workweave surfaces its own copy of the\n\
             // notes.md the operator declared, so the reap stays owner-scoped.\n\
             pub fn f() {}\n",
        );
        assert!(
            errors.is_empty(),
            "a wrapped sentence must not manufacture a citation, got:\n{}",
            errors.join("\n")
        );
    }

    // ── operator-surface internals check unit tests ─────────────────────────

    /// Write a `docs/SUMMARY.md` linking to one clean page, plus the page
    /// itself, so the summary-derived slice of the scan is non-empty and
    /// contributes no finding of its own. Tests that seed a finding
    /// elsewhere call this first, so the non-vacuity guard does not add an
    /// unrelated entry to `errors` and the assertion stays about the one
    /// finding under test.
    fn write_minimal_clean_summary(root: &Path) {
        let docs = root.join("docs");
        fs::create_dir_all(&docs).unwrap();
        fs::write(
            docs.join("SUMMARY.md"),
            "# Summary\n\n- [Intro](./intro.md)\n",
        )
        .unwrap();
        fs::write(docs.join("intro.md"), "# intro\n\nNothing to see here.\n").unwrap();
    }

    /// A `docs/internals/` reference on an assembled explain page is caught.
    /// This is the seeded-failure test: the gate must report a fixture whose
    /// content it is meant to reject, not merely stay quiet on a clean tree.
    #[test]
    fn internals_path_on_assembled_explain_page_is_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        write_minimal_clean_summary(root);
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
        write_minimal_clean_summary(root);
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
        write_minimal_clean_summary(root);
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

    /// A clean tree — assembled explain page, generated schema, and a
    /// summary-listed hand-authored page — passes across the whole widened
    /// scope.
    #[test]
    fn clean_operator_surfaces_are_allowed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        write_minimal_clean_summary(root);
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

    /// A `docs/internals/` reference on a hand-authored page `docs/SUMMARY.md`
    /// lists is caught — the half of the scan that reads pages the sidebar
    /// reaches rather than pages the generator writes. The anchor assertion
    /// checks the specific page name, not merely that the scan produced *a*
    /// finding, so a scan that matched the wrong file would not pass
    /// vacuously.
    #[test]
    fn internals_path_on_summary_listed_page_is_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let docs = root.join("docs");
        fs::create_dir_all(docs.join("how-to")).unwrap();
        fs::write(
            docs.join("SUMMARY.md"),
            "# Summary\n\n- [Guide](./how-to/guide.md)\n",
        )
        .unwrap();
        fs::write(
            docs.join("how-to/guide.md"),
            "# guide\n\nSee docs/internals/branch-model.md for the rule set.\n",
        )
        .unwrap();
        let errors = check_no_internals_on_operator_surfaces(root);
        let combined = errors.join("\n");
        assert!(
            slashed(&combined).contains("how-to/guide.md") && combined.contains("docs/internals/"),
            "an internals reference on a summary-listed page must be reported \
             against that page, got:\n{combined}"
        );

        // Remove the violation: the same page, now clean, must pass. This is
        // the anchor's other half — the finding tracks the content, not the
        // fact that a file with this name exists.
        fs::write(
            docs.join("how-to/guide.md"),
            "# guide\n\nNothing to see here.\n",
        )
        .unwrap();
        let errors_after_fix = check_no_internals_on_operator_surfaces(root);
        assert!(
            errors_after_fix.is_empty(),
            "removing the internals reference must clear the finding, got:\n{}",
            errors_after_fix.join("\n")
        );
    }

    /// Root `README.md` and `ARCHITECTURE.md` are not `docs/SUMMARY.md`-listed
    /// pages and are read on GitHub directly, where `docs/internals/` links
    /// resolve — both already reference it deliberately. The scan must not
    /// reach them even when they carry the same text a SUMMARY-listed page
    /// would be reported for.
    #[test]
    fn readme_and_architecture_internals_mentions_are_out_of_scope() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        write_minimal_clean_summary(root);
        fs::write(
            root.join("README.md"),
            "# repoweave\n\nSee docs/internals/branch-model.md.\n",
        )
        .unwrap();
        fs::write(
            root.join("ARCHITECTURE.md"),
            "# architecture\n\nSee docs/internals/concurrency.md.\n",
        )
        .unwrap();
        let errors = check_no_internals_on_operator_surfaces(root);
        assert!(
            errors.is_empty(),
            "README.md and ARCHITECTURE.md must stay out of scope, got:\n{}",
            errors.join("\n")
        );
    }

    /// Non-vacuity floor: a `docs/SUMMARY.md` that resolves to zero pages —
    /// here, missing entirely — must fail the gate rather than silently
    /// scanning nothing.
    #[test]
    fn zero_summary_pages_is_a_reported_failure() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::create_dir_all(root.join("docs/reference/explain")).unwrap();
        // No docs/SUMMARY.md at all.
        let errors = check_no_internals_on_operator_surfaces(root);
        assert!(
            errors.iter().any(|e| e.contains("yielded zero pages")),
            "a missing docs/SUMMARY.md must fail the non-vacuity floor, got:\n{}",
            errors.join("\n")
        );
    }

    /// `docs/SUMMARY.md` links are parsed, not hand-enumerated: nested
    /// (indented) entries are found, a `#`-anchor-only target and an
    /// absolute URL are skipped as not being pages, and a leading `./` does
    /// not survive into the resolved path.
    #[test]
    fn collect_summary_listed_pages_parses_nested_and_skips_non_pages() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let docs = root.join("docs");
        fs::create_dir_all(docs.join("reference/integrations")).unwrap();
        fs::write(
            docs.join("SUMMARY.md"),
            "# Summary\n\n\
             - [Intro](./introduction.md)\n\
             - [Integrations](./reference/integrations/index.md)\n  \
               - [npm](./reference/integrations/npm-workspaces.md)\n\
             - [Anchor only](#not-a-page)\n\
             - [External](https://example.com/not-a-page.md)\n",
        )
        .unwrap();
        let pages = collect_summary_listed_pages(root);
        assert_eq!(
            pages,
            vec![
                docs.join("introduction.md"),
                docs.join("reference/integrations/index.md"),
                docs.join("reference/integrations/npm-workspaces.md"),
            ],
            "expected exactly the three .md targets, sorted, got:\n{pages:?}"
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
    fn bare_identifiers_are_not_mechanized() {
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
- [Lock-as-derived](./explanation/joints/lock-as-derived.md)\n\
- [Clone topology](./explanation/joints/clone-topology.md)\n";
        let index = "\
- [Lock-as-derived](./joints/lock-as-derived.md)\n\
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
- [Lock-as-derived](./explanation/joints/lock-as-derived.md)\n\
- [Workweave lifecycle](./explanation/joints/workweave-lifecycle.md)\n";
        let index = "\
- [Lock-as-derived](./joints/lock-as-derived.md)\n";
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
- [Lock-as-derived](./explanation/joints/lock-as-derived.md)\n";
        let index = "\
- [Lock-as-derived](./joints/lock-as-derived.md)\n\
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
- [Lock-as-derived](./explanation/joints/lock-as-derived.md)\n";
        let stems = collect_joints_stems_from_summary(summary);
        assert_eq!(
            stems.len(),
            1,
            "only the joints line should be extracted, got: {stems:?}"
        );
        assert!(stems.contains("lock-as-derived"));
    }

    /// Non-joints lines in index.md (lenses section) are not extracted.
    #[test]
    fn index_stem_extractor_ignores_non_joints_lines() {
        let index = "\
- [Workspace lens](./lenses/workspace.md)\n\
- [Lock-as-derived](./joints/lock-as-derived.md)\n";
        let stems = collect_joints_stems_from_index(index);
        assert_eq!(
            stems.len(),
            1,
            "only the joints line should be extracted, got: {stems:?}"
        );
        assert!(stems.contains("lock-as-derived"));
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

    // ── vocabulary-and-spelling check unit tests ────────────────────────────

    /// One clean text in every surface class, so the non-vacuity floor stays
    /// quiet and each test's assertion is about its own seed. Returns the help
    /// pages to scan alongside the tree.
    fn write_minimal_prose_tree(root: &Path) -> Vec<(String, String)> {
        write_minimal_clean_summary(root);
        for (rel, body) in [
            (
                "README.md",
                "# repoweave\n\nA weave root holds member repos.\n",
            ),
            ("docs/reference/explain/sync.md", "# sync\n"),
            ("docs/reference/explain/templates/sync.md.tmpl", "# sync\n"),
            ("docs/reference/prime/overview.md", "# prime\n"),
            ("docs/reference/schemas/sync.json", "{}\n"),
            ("src/lib.rs", "pub fn f() {}\n"),
            ("tests/t.rs", "#[test]\nfn t() {}\n"),
        ] {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        }
        vec![("rwv --help".to_owned(), "Usage: rwv\n".to_owned())]
    }

    fn write_at(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn clean_prose_tree_passes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let help = write_minimal_prose_tree(tmp.path());
        let errors = check_vocabulary_and_spelling(tmp.path(), &help);
        assert!(errors.is_empty(), "got:\n{}", errors.join("\n"));
    }

    /// The seeded-failure pin for the vocabulary half, on a page the sidebar
    /// lists. Fixing the page clears the finding, so the finding tracks the
    /// content rather than the file.
    #[test]
    fn banned_synonym_on_summary_listed_page_is_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let help = write_minimal_prose_tree(root);
        write_at(
            root,
            "docs/intro.md",
            "# intro\n\nEach manifest repo is cloned.\n",
        );
        let errors = check_vocabulary_and_spelling(root, &help);
        assert!(
            errors.iter().any(|e| slashed(e).starts_with(
                "docs/intro.md:3: `manifest repo` — canonical vocabulary: member repo"
            )),
            "got:\n{}",
            errors.join("\n")
        );
        write_at(
            root,
            "docs/intro.md",
            "# intro\n\nEach member repo is cloned.\n",
        );
        let errors = check_vocabulary_and_spelling(root, &help);
        assert!(errors.is_empty(), "got:\n{}", errors.join("\n"));
    }

    /// A phrase wrapped across lines is one phrase to a reader, and the
    /// finding names the line it starts on — the line an author edits.
    #[test]
    fn wrapped_synonym_is_reported_at_the_line_it_starts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let help = write_minimal_prose_tree(root);
        write_at(
            root,
            "docs/intro.md",
            "# intro\n\nThe lock pins every manifest\n  repo to a SHA, at the workspace\nroot.\n",
        );
        let errors = check_vocabulary_and_spelling(root, &help);
        let combined = slashed(&errors.join("\n"));
        assert!(
            combined.contains("docs/intro.md:3: `manifest repo`")
                && combined.contains("docs/intro.md:4: `workspace root`"),
            "got:\n{combined}"
        );
    }

    /// The seeded-failure pin for the spelling half, planted in a template:
    /// the gate reads templates as well as what is generated from them.
    #[test]
    fn british_spelling_in_explain_template_is_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let help = write_minimal_prose_tree(root);
        write_at(
            root,
            "docs/reference/explain/templates/sync.md.tmpl",
            "# sync\n\nThe default behaviour is a fast-forward.\n",
        );
        let errors = check_vocabulary_and_spelling(root, &help);
        assert!(
            errors.iter().any(|e| slashed(e).starts_with(
                "docs/reference/explain/templates/sync.md.tmpl:3: `behaviour` — American spelling"
            )),
            "got:\n{}",
            errors.join("\n")
        );
    }

    /// Generated schemas and rendered `--help` are scanned as they are
    /// served, each reported under its own name.
    #[test]
    fn banned_terms_in_generated_schema_and_help_are_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let mut help = write_minimal_prose_tree(root);
        write_at(
            root,
            "docs/reference/schemas/sync.json",
            "{\"description\": \"Primary workspace root, in the wire spelling.\"}\n",
        );
        help.push((
            "rwv resolve --help".to_owned(),
            "Print the path\n\nUsage: rwv resolve\n\nNormalises nothing.\n".to_owned(),
        ));
        let errors = check_vocabulary_and_spelling(root, &help);
        let combined = slashed(&errors.join("\n"));
        assert!(
            combined.contains("docs/reference/schemas/sync.json:1: `workspace root`")
                && combined.contains("rwv resolve --help:5: `Normalises`"),
            "got:\n{combined}"
        );
    }

    /// Message text is the code of a production region: a literal continued
    /// with `\` is read as the one string it is, while a comment and a test
    /// module answer to spelling only.
    #[test]
    fn src_message_text_is_held_to_the_vocabulary() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let help = write_minimal_prose_tree(root);
        write_at(
            root,
            "src/lib.rs",
            "pub fn f() -> anyhow::Result<()> {\n\
             \x20   // A manifest repo that failed is named above.\n\
             \x20   anyhow::bail!(\"push failed for one or more manifest \\\n\
             \x20                  repos; see above\")\n\
             }\n\
             \n\
             #[cfg(test)]\n\
             mod tests {\n\
             \x20   const MSG: &str = \"manifest repo\";\n\
             }\n",
        );
        let errors = check_vocabulary_and_spelling(root, &help);
        assert_eq!(
            errors.iter().map(|e| slashed(e)).collect::<Vec<_>>(),
            vec!["src/lib.rs:3: `manifest repos` — canonical vocabulary: member repo".to_owned()],
        );
    }

    /// Spelling reaches what vocabulary does not: a `src/` comment, a test,
    /// and a maintainer page under `docs/internals/`.
    #[test]
    fn spelling_reaches_comments_tests_and_internals() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let help = write_minimal_prose_tree(root);
        write_at(
            root,
            "src/lib.rs",
            "// The writer serialises access.\npub fn f() {}\n",
        );
        write_at(
            root,
            "tests/t.rs",
            "#[test]\nfn t() {\n    // recognised\n}\n",
        );
        write_at(
            root,
            "docs/internals/notes.md",
            "# notes\n\nA manifest repo with modelled behaviour.\n",
        );
        let errors = check_vocabulary_and_spelling(root, &help);
        let combined = slashed(&errors.join("\n"));
        for want in [
            "src/lib.rs:1: `serialises`",
            "tests/t.rs:3: `recognised`",
            "docs/internals/notes.md:3: `modelled`",
            "docs/internals/notes.md:3: `behaviour`",
        ] {
            assert!(combined.contains(want), "missing {want}, got:\n{combined}");
        }
        assert!(
            !combined.contains("canonical vocabulary"),
            "docs/internals answers to spelling only, got:\n{combined}"
        );
    }

    /// Words a looser matcher would report: the `-ise`, `-our`, `-ll` and
    /// `-ys` look-alikes, the workweave's own directory, an ecosystem's
    /// workspace root, `git-repo`'s manifest repository, and identifiers.
    #[test]
    fn look_alikes_are_not_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let help = write_minimal_prose_tree(root);
        write_at(
            root,
            "docs/intro.md",
            "# intro\n\nOtherwise the premise of the exercise: organism, synthesis,\n\
             analyses, mechanism, criticism, emphasis, parenthesis, cancellation,\n\
             an hour-long detour, totally equally. The workweave directory is\n\
             removed. A nested workspace root, a virtual workspace root, a Cargo\n\
             workspace root and `multiple workspace roots found in the same\n\
             workspace` are each tool's own. A `git-repo` manifest repository is\n\
             not ours either.\n",
        );
        write_at(
            root,
            "src/lib.rs",
            "pub struct NormalisedPath;\npub fn serialised_form() -> &'static str { \"x\" }\n",
        );
        let errors = check_vocabulary_and_spelling(root, &help);
        assert!(errors.is_empty(), "got:\n{}", errors.join("\n"));
    }

    /// `CLAUDE.md` states both rules, so it names what they ban.
    #[test]
    fn claude_md_is_out_of_scope() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let help = write_minimal_prose_tree(root);
        write_at(root, "CLAUDE.md", "Not \"manifest repo\"; not whilst.\n");
        let errors = check_vocabulary_and_spelling(root, &help);
        assert!(errors.is_empty(), "got:\n{}", errors.join("\n"));
    }

    /// Non-vacuity: every surface class that yields nothing is a finding.
    #[test]
    fn empty_surface_classes_are_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let errors = check_vocabulary_and_spelling(tmp.path(), &[]);
        let (_, population) = vocabulary_and_spelling_texts(tmp.path(), &[]);
        assert_eq!(
            errors
                .iter()
                .filter(|e| e.contains("yielded nothing"))
                .count(),
            population.len(),
            "got:\n{}",
            errors.join("\n")
        );
    }

    /// This file's spelling table and test module are masked from its own
    /// scan; a spelling anywhere else in it is still reported.
    #[test]
    fn self_table_and_test_module_are_masked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let help = write_minimal_prose_tree(root);
        write_at(
            root,
            "src/bin/generate-explain.rs",
            "const BRITISH_SPELLINGS: &[&str] = &[\n    \"behaviour\",\n];\n\
             // honour this\n\
             #[cfg(test)]\n\
             mod tests {\n    // judgement\n}\n",
        );
        let errors = check_vocabulary_and_spelling(root, &help);
        assert_eq!(
            errors.iter().map(|e| slashed(e)).collect::<Vec<_>>(),
            vec![
                "src/bin/generate-explain.rs:4: `honour` — American spelling: -or, not -our"
                    .to_owned()
            ],
        );
    }

    /// Every command's `--help` is read: one page for the root and one per
    /// subcommand path in clap's tree, none of them empty.
    #[test]
    fn help_pages_cover_every_command() {
        let cli = Cli::command();
        let pages = rendered_help_pages(&cli);
        assert_eq!(pages.len(), collect_subcommand_paths(&cli).len() + 1);
        assert!(pages.iter().all(|(_, text)| !text.trim().is_empty()));
        assert!(pages
            .iter()
            .any(|(name, _)| name == "rwv workweave create --help"));
    }

    /// The scan population of this repository, by surface class. Run with
    /// `--nocapture` to read the counts.
    #[test]
    fn prose_scan_reads_every_surface_class_of_this_repository() {
        let root = repo_root();
        let help = rendered_help_pages(&Cli::command());
        let (texts, population) = vocabulary_and_spelling_texts(&root, &help);
        for (class, count) in &population {
            println!("{class}: {count}");
            assert!(*count > 0, "{class} yielded nothing");
        }
        let lines: usize = texts.iter().map(|t| t.text.lines().count()).sum();
        println!("lines read: {lines}");
    }

    /// The tokens the doc-token fixtures below seed, none of which the code
    /// defines.
    const SEEDED_PHANTOMS: &[&str] = &[
        "user-held-featured",
        "orphaned-owned-region",
        "fully-owned-digest-mismatch",
        "dep-version-skew",
        "patch-shadowed",
        "fetch-realign-refused",
        "integration-file-drifted",
        "canonical-wrong-branch",
        "rebase-aborted",
        "deactivate",
    ];

    fn real_token_vocabulary() -> (TokenVocabulary, clap::Command) {
        let cli = built_cli(&Cli::command());
        (token_vocabulary(&repo_root(), &cli), cli)
    }

    /// `body` as the one page a temp tree's `docs/SUMMARY.md` lists, scanned
    /// against this repository's vocabulary and clap tree.
    fn scan_page(body: &str) -> (Vec<String>, DocTokenPopulation) {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_at(
            tmp.path(),
            "docs/SUMMARY.md",
            "# Summary\n\n- [Page](./page.md)\n",
        );
        write_at(tmp.path(), "docs/page.md", body);
        let (vocab, cli) = real_token_vocabulary();
        let (findings, population) = scan_doc_tokens(tmp.path(), &vocab, &cli);
        (findings.iter().map(|f| slashed(f)).collect(), population)
    }

    fn assert_reported(findings: &[String], wanted: &[&str]) {
        for want in wanted {
            assert!(
                findings.iter().any(|f| f.starts_with(want)),
                "missing `{want}`, got:\n{}",
                findings.join("\n")
            );
        }
        assert_eq!(
            findings.len(),
            wanted.len(),
            "got:\n{}",
            findings.join("\n")
        );
    }

    /// Lines the operator pages once shipped, verbatim, each citing a finding
    /// name, refusal token, sub-kind or subcommand that never existed. Every
    /// invention is reported at its own line, and the sub-kind once — as a
    /// sub-kind of the kind it was written under.
    #[test]
    fn the_shipped_inventions_are_each_reported() {
        let page = "# page\n\
            \n\
            - **`USER-HELD`** (`user-held-featured`, warning, not auto-fixable): `folders` is present without the `\"rwv.generated\"` marker.\n\
            - When disabled (`enabled = false`), leftover `gita/repos.csv` or `gita/groups.csv` files in `projects/<project>/gita/` are reported as `orphaned-owned-region` (`DRIFT` if their bytes match `rwv`'s output, or `USER-HELD` if modified by hand).\n\
            - A lock that is present and parseable in `projects/<project>/Cargo.lock` whose bytes differ from the **last `rwv`-accepted generation** is reported as a warning (`fully-owned-digest-mismatch`, not auto-fixed).\n\
            - **Dependency version skew and patch shadowing**: `rwv doctor` also scans workspace members (and nested workspaces) for inconsistent dependency version requirements (`dep-version-skew`) and member `.cargo/config.toml` files that shadow weave-level patches (`patch-shadowed`).\n\
            \n\
            | Present, locked SHA is not a fast-forward or checkout is on another branch | Refuses (`fetch-realign-refused`), unless `--detach-checkouts` is passed to materialize the pin on a detached HEAD (use `git checkout <branch>` later before running `rwv sync-to`) |\n\
            | 2. Drift settlement | Re-derives generated files whose inputs changed (`derived-state-stale`) and drops attestations for files no enabled integration generates. If an attested file's bytes differ from the accepted generation (`integration-file-drifted`), refuses unless `--regenerate-drifted` or `--adopt-drifted` is passed |\n\
            \n\
            1. `rwv doctor` reports [`branch-discipline`](../../reference/doctor-findings.md#branch-discipline) (`canonical-wrong-branch`).\n\
            \n\
            `rwv doctor` and context verbs (`rwv activate`, `rwv fetch`, `rwv workweave add`) verify `gita/repos.csv` and `gita/groups.csv`:\n";
        let (findings, _) = scan_page(page);
        assert_reported(
            &findings,
            &[
                "docs/page.md:3: `user-held-featured` is not a token",
                "docs/page.md:4: `orphaned-owned-region` is not a token",
                "docs/page.md:5: `fully-owned-digest-mismatch` is not a token",
                "docs/page.md:6: `dep-version-skew` is not a token",
                "docs/page.md:6: `patch-shadowed` is not a token",
                "docs/page.md:8: `fetch-realign-refused` is not a token",
                "docs/page.md:9: `integration-file-drifted` is not a token",
                "docs/page.md:11: `canonical-wrong-branch` is not a sub-kind of `branch-discipline`",
                "docs/page.md:13: `rwv workweave add` is not a command",
            ],
        );
    }

    /// One seed per recognition branch the shipped inventions do not reach:
    /// a failure kind, a verb at the root, a subcommand after a positional, a
    /// subcommand in a fenced block, a real sub-kind under the wrong kind, and
    /// the token arguments of `rwv explain` and `rwv doctor --kind`.
    #[test]
    fn a_phantom_on_each_remaining_branch_is_reported() {
        let page = concat!(
            "# page\n",
            "\n",
            "`rwv sync` names the repo `rebase-aborted`.\n",
            // rwv-advice: not-an-invocation
            "Run `rwv deactivate` first.\n",
            // rwv-advice: not-an-invocation
            "Then `rwv workweave web-app add pr-42`.\n",
            "The row reads `weave-root-identity-conflict` / `detached`.\n",
            "Read `rwv explain integration-file-drifted`, then\n",
            "`rwv doctor --kind fully-owned-digest-mismatch` and `rwv doctor --kind=stale-lok`.\n",
            "\n",
            "```bash\n",
            "$ rwv workweave add pr-42   # make one\n",
            "```\n",
        );
        let (findings, _) = scan_page(page);
        assert_reported(
            &findings,
            &[
                "docs/page.md:3: `rebase-aborted` is not a token",
                // rwv-advice: not-an-invocation
                "docs/page.md:4: `rwv deactivate` is not a verb, and this page names no `rwv-deactivate` plugin",
                // rwv-advice: not-an-invocation
                "docs/page.md:5: `rwv workweave web-app add` is not a command",
                "docs/page.md:6: `detached` is not a sub-kind of `weave-root-identity-conflict`",
                "docs/page.md:7: `rwv explain integration-file-drifted` serves nothing",
                "docs/page.md:8: `rwv doctor --kind fully-owned-digest-mismatch` refuses",
                "docs/page.md:8: `rwv doctor --kind stale-lok` refuses",
                // rwv-advice: not-an-invocation
                "docs/page.md:11: `rwv workweave add pr-42` is not a command",
            ],
        );
    }

    /// What a looser scan would report: real tokens of every class, a
    /// positional before its subcommand, global options, a plugin the page
    /// names, a bundled plugin, an absent verb, placeholders, output lines, a list that stops at
    /// a span not shaped like a token, words the code derives through clap and
    /// serde, and the exemptions.
    #[test]
    fn real_tokens_and_look_alikes_are_not_reported() {
        let page = concat!(
            "# page\n",
            "\n",
            "`stale-lock`, `tool-missing`, `push-from-workweave`, `rebase-failed`,\n",
            "[`branch-discipline`](#x) (`detached`), `missing-replay-exclusion` (`absent`,\n",
            "`legacy-spelling`), `workweave-drift` (`.rwv-workweave` is gone).\n",
            "`rwv workweave web-app create pr-42`, `rwv workweave <project> list`,\n",
            "`rwv -w myproj--hotfix sync primary`, `rwv workweave\n",
            // rwv-advice: not-an-invocation
            "create`, `rwv tidy` from `rwv-tidy`, no `rwv commit`, `rwv <verb>`,\n",
            "`rwv --help`, `rwv explain stale-lock`, `rwv explain sync-to`,\n",
            // rwv-advice: not-an-invocation
            "`rwv explain <token>`, `rwv doctor --kind tool-missing`, `rwv check-mid-op && rwv sync-to`.\n",
            "`sync-to`, `set-container`, `allow-stale-lock`, `cargo-config`, `up-to-date`, `web-app`,\n",
            "`rwv-lint`, `owner/repo`, `--allow-stale-lock`, `project-rwv--agent-home`.\n",
            // rwv-advice: not-an-invocation
            "`rwv each --role fork -- git status` marks a repo `not-present`.\n",
            "\n",
            "```text\n",
            "rwv push: 1 repo(s) disagree with rwv.lock:\n",
            "[error] core: x is a YAML manifest; rwv reads rwv.toml.\n",
            "```\n",
            "\n",
            "```\n",
            "rwv check-mid-op   # runs rwv-check-mid-op\n",
            "```\n",
        );
        let (findings, population) = scan_page(page);
        assert!(findings.is_empty(), "got:\n{}", findings.join("\n"));
        assert_eq!(population.positional_before_subcommand, 1);
        assert_eq!(population.qualified_sub_kinds, 3);
        assert_eq!(population.plugin_or_absent_verbs, 5);
    }

    /// A span closes at the next run of exactly as many backticks, may wrap
    /// a line but not leave its paragraph, and an unpartnered run is text;
    /// fenced lines, under either fence character and at any indent, are kept
    /// apart from the prose.
    #[test]
    fn code_spans_pair_their_backticks_within_a_paragraph() {
        let code = page_code(
            "A `` `kind` `` span and a stray `` run.\n\
             `wrapped\n\
             span` here.\n\
             \n\
             `open\n\
             \n\
             close`\n\
             - item\n\
             \x20 ~~~\n\
             \x20 rwv sync\n\
             \x20 ~~~\n",
        );
        let spans: Vec<(usize, &str)> = code
            .spans
            .iter()
            .map(|s| (s.line, s.text.as_str()))
            .collect();
        assert_eq!(spans, [(1, "`kind`"), (2, "wrapped span")]);
        assert_eq!(code.fenced, [(10, "  rwv sync".to_owned())]);
    }

    #[test]
    fn refusal_tokens_honor_a_serde_rename() {
        let source = "pub enum RefusalKind {\n    /// Doc.\n    VersionIsAPin,\n    \
                      #[serde(rename = \"spelled-out\")]\n    Renamed,\n}\n";
        let tokens = refusal_tokens(source);
        assert_eq!(
            tokens,
            HashSet::from(["version-is-a-pin".to_owned(), "spelled-out".to_owned()])
        );
        let real = refusal_tokens(&fs::read_to_string(repo_root().join("src/refusal.rs")).unwrap());
        assert!(real.len() > 90, "read {} refusal tokens", real.len());
        assert!(real.contains(&repoweave::refusal::RefusalKind::VersionIsAPin.token()));
    }

    #[test]
    fn serde_kebab_names_read_variants_and_fields() {
        let source = "#[derive(Deserialize)]\n\
             #[serde(rename_all = \"kebab-case\")]\n\
             pub enum Surface {\n\
             \x20   /// Doc.\n\
             \x20   CargoConfig,\n\
             \x20   Shaped { inner_field: u8 },\n\
             \x20   Wide {\n\
             \x20       nested_field: u8,\n\
             \x20   },\n\
             \x20   #[serde(rename = \"own\")]\n\
             \x20   Renamed,\n\
             }\n\
             #[serde(deny_unknown_fields, rename_all = \"kebab-case\")]\n\
             struct Settings {\n\
             \x20   pub rustc_wrapper: Option<String>,\n\
             \x20   hide_dotfiles: bool,\n\
             }\n\
             enum Unrenamed {\n\
             \x20   NotRead,\n\
             }\n";
        assert_eq!(
            serde_kebab_names(source),
            [
                "cargo-config",
                "shaped",
                "wide",
                "rustc-wrapper",
                "hide-dotfiles"
            ]
        );
    }

    /// Every class and branch under its floor is a finding, so a scan that
    /// stops reading one cannot pass for one that read it clean.
    #[test]
    fn a_class_under_its_floor_is_reported() {
        let findings = floor_findings(&DocTokenPopulation::default());
        assert_eq!(findings.len(), DocTokenPopulation::default().floors().len());
        assert!(findings
            .iter()
            .any(|f| f.starts_with("doc-token scan: refusal tokens yielded 0 citations")));
    }

    /// The scan population of this repository's operator pages, every class
    /// and branch at or above its floor. Run with `--nocapture` to read the
    /// counts.
    #[test]
    fn doc_token_scan_reaches_every_class_and_branch_of_this_repository() {
        let (vocab, cli) = real_token_vocabulary();
        let (_, population) = scan_doc_tokens(&repo_root(), &vocab, &cli);
        for (class, count, floor) in population.floors() {
            println!("{class}: {count} (floor {floor})");
        }
        let under = floor_findings(&population);
        assert!(under.is_empty(), "{}", under.join("\n"));
    }

    /// The vocabulary is read from production code outside this file, so the
    /// phantoms this module seeds stay phantoms.
    #[test]
    fn the_seeded_phantoms_are_defined_nowhere() {
        let (vocab, cli) = real_token_vocabulary();
        for seed in SEEDED_PHANTOMS {
            assert!(!vocab.defines(seed), "the vocabulary defines `{seed}`");
            assert!(
                cli.find_subcommand(seed).is_none(),
                "`rwv {seed}` is a verb"
            );
        }
    }

    /// A word the `rwv-` prefix exempts can never be a condition token, and
    /// each exemption stays only while the code does not define it and a page
    /// still cites it.
    #[test]
    fn every_exemption_is_still_needed() {
        let (vocab, cli) = real_token_vocabulary();
        for token in vocab
            .finding_kinds
            .iter()
            .chain(&vocab.refusals)
            .chain(&vocab.failure_kinds)
            .chain(vocab.sub_kinds.values().flatten())
        {
            assert!(
                !token.starts_with("rwv-"),
                "`{token}` is spelled like a plugin"
            );
        }
        let mut cited = HashSet::new();
        for path in doc_token_pages(&repo_root()) {
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            for span in page_code(&text).spans {
                cited.extend(
                    rwv_invocations(&span.text)
                        .iter()
                        .filter_map(|w| w.first().map(|v| v.to_string())),
                );
                cited.insert(span.text);
            }
        }
        for (word, _) in NON_TOKEN_WORDS {
            assert!(
                !vocab.defines(word),
                "the code defines `{word}`; drop the exemption"
            );
            assert!(
                cited.contains(*word),
                "no page cites `{word}`; drop the exemption"
            );
        }
        for verb in ABSENT_VERBS {
            assert!(
                cli.find_subcommand(verb).is_none(),
                "`rwv {verb}` exists; drop the exemption"
            );
            assert!(
                cited.contains(*verb),
                "no page cites `rwv {verb}`; drop the exemption"
            );
        }
    }

    fn plugin_tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        for (name, body) in files {
            let path = tmp.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        }
        tmp
    }

    #[test]
    fn plugin_boundary_passes_a_plugin_that_never_names_the_library() {
        let tmp = plugin_tree(&[
            ("main.rs", "mod run;\nfn main() { run::go(); }\n"),
            ("run.rs", "pub fn go() { let _ = serde_json::json!({}); }\n"),
        ]);
        let errors = check_plugin_library_boundary(tmp.path(), LIBRARY_CRATE);
        assert!(errors.is_empty(), "{}", errors.join("\n"));
    }

    #[test]
    fn plugin_boundary_fails_on_a_use_of_the_library() {
        let seeded = format!("use {LIBRARY_CRATE}::manifest::Role;\n");
        let tmp = plugin_tree(&[("main.rs", "fn main() {}\n"), ("nested/sel.rs", &seeded)]);
        let errors = check_plugin_library_boundary(tmp.path(), LIBRARY_CRATE);
        assert_eq!(errors.len(), 1, "{}", errors.join("\n"));
        assert!(
            slashed(&errors[0]).contains("nested/sel.rs:1") && errors[0].contains("names the"),
            "{}",
            errors[0]
        );
    }

    #[test]
    fn plugin_boundary_fails_on_every_way_of_naming_the_library_crate() {
        for seeded in [
            format!("use {LIBRARY_CRATE};\n"),
            format!("use {LIBRARY_CRATE} as lib;\n"),
            format!("extern crate {LIBRARY_CRATE};\n"),
            format!("use {{std::fs, {LIBRARY_CRATE}::status}};\n"),
        ] {
            let tmp = plugin_tree(&[("main.rs", &seeded)]);
            let errors = check_plugin_library_boundary(tmp.path(), LIBRARY_CRATE);
            assert_eq!(errors.len(), 1, "{seeded:?}: {}", errors.join("\n"));
        }
    }

    #[test]
    fn plugin_boundary_ignores_the_crate_name_in_a_url_or_prose() {
        let body = format!(
            "// see the {LIBRARY_CRATE} book\nconst U: &str = \"https://example.test/{LIBRARY_CRATE}/x.json\";\n"
        );
        let tmp = plugin_tree(&[("main.rs", &body)]);
        let errors = check_plugin_library_boundary(tmp.path(), LIBRARY_CRATE);
        assert!(errors.is_empty(), "{}", errors.join("\n"));
    }

    #[test]
    fn plugin_boundary_fails_on_a_path_qualified_call_into_the_library() {
        let seeded = format!("fn f() {{ ::{LIBRARY_CRATE}::status::run(); }}\n");
        let tmp = plugin_tree(&[("main.rs", &seeded)]);
        assert_eq!(
            check_plugin_library_boundary(tmp.path(), LIBRARY_CRATE).len(),
            1
        );
    }

    #[test]
    fn plugin_boundary_fails_on_compiling_in_another_files_source() {
        for seeded in [
            "#[path = \"../../selector.rs\"]\nmod selector;\n",
            "include!(\"../../selector.rs\");\n",
            "const S: &str = include_str!(\"../../lib.rs\");\n",
        ] {
            let tmp = plugin_tree(&[("main.rs", seeded)]);
            let errors = check_plugin_library_boundary(tmp.path(), LIBRARY_CRATE);
            assert_eq!(errors.len(), 1, "{seeded:?}: {}", errors.join("\n"));
            assert!(errors[0].contains("compiles in source"), "{}", errors[0]);
        }
    }

    #[test]
    fn plugin_boundary_refuses_a_missing_directory_rather_than_passing_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let errors = check_plugin_library_boundary(&tmp.path().join("absent"), LIBRARY_CRATE);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("not a directory"), "{}", errors[0]);
    }

    #[test]
    fn plugin_boundary_holds_on_the_shipped_plugin_source() {
        let root = repo_root();
        for dir in PLUGIN_SOURCE_DIRS {
            let errors = check_plugin_library_boundary(&root.join(dir), LIBRARY_CRATE);
            assert!(errors.is_empty(), "{}", errors.join("\n"));
        }
    }
}
