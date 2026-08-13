//! A draft-07 validator over the keyword subset schemars emits, plus readers
//! for the committed artifacts under `docs/reference/schemas/`.
//!
//! Every `--json` verb embeds a `$schema` URL naming one of those artifacts.
//! Regenerating an artifact proves it matches the Rust type; it does not prove
//! the bytes a verb writes to stdout match the artifact. This module is the
//! shared instrument for asking the second question, so that asking it about a
//! seventh verb does not mean a seventh validator.
//!
//! # What the validator covers, and what it refuses to
//!
//! It implements `$ref`, `type`, `enum`, `required`, `properties`,
//! `additionalProperties`, `items`, `oneOf`, `anyOf`, `allOf`, `format` and
//! `minimum`. Every other keyword is **reported as an error**, not ignored. A
//! validator that skips what it does not understand green-lights the part it
//! looked at and reads as green over the part it did not — so if schemars ever
//! starts emitting `pattern` or `minItems`, this stops the tree until someone
//! implements it. [`census`] is the same rule applied to an artifact ahead of
//! any instance.
//!
//! Two deliberate departures from draft-07 semantics, both tightening:
//!
//!   1. An object schema is treated as closed whenever it declares
//!      `properties` or `additionalProperties`. Draft-07 would allow undeclared
//!      keys through, and an added key is precisely the shape a hand-minted
//!      envelope takes. Every object in these schemas comes from a Rust struct
//!      or enum variant, so nothing legitimate is rejected: a new field moves
//!      the artifact and the artifact is what this reads.
//!   2. A boolean schema (`true` / `false` in schema position) is an error
//!      rather than always-pass / always-fail. schemars does not emit them.
//!
//! `format` is checked rather than annotated. Draft-07 leaves it advisory, but
//! the only values in these artifacts are schemars' numeric widths, and
//! `uint` is the difference between `commits_ahead: 3` and `commits_ahead: -3`
//! — a distinction `type: integer` does not draw.

use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Keywords that constrain nothing. Anything absent from both this list and
/// the match in [`apply_keyword`] is reported rather than skipped.
pub const ANNOTATIONS: &[&str] = &[
    "$schema",
    "$id",
    "title",
    "description",
    "default",
    "examples",
    "definitions",
];

/// The keywords [`apply_keyword`] enforces. Kept as data so a test can assert
/// an artifact stays inside the subset without restating the list.
pub const IMPLEMENTED: &[&str] = &[
    "$ref",
    "type",
    "enum",
    "required",
    "properties",
    "additionalProperties",
    "items",
    "oneOf",
    "anyOf",
    "allOf",
    "format",
    "minimum",
];

// ---------------------------------------------------------------------------
// Reading the committed artifacts
// ---------------------------------------------------------------------------

/// `docs/reference/schemas/`, resolved from the crate root rather than the
/// process cwd.
pub fn schema_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/reference/schemas")
}

/// Repo-relative path of `verb`'s committed artifact, for failure messages.
pub fn schema_path(verb: &str) -> String {
    format!("docs/reference/schemas/{verb}.json")
}

/// Every verb with a committed artifact, read from the directory itself.
///
/// The listing is the input so a newly committed schema arrives in whatever
/// test consumes this, rather than in a list someone has to remember to edit.
pub fn committed_verbs() -> Vec<String> {
    let dir = schema_dir();
    let mut verbs: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{} is unreadable: {e}", dir.display()))
        .map(|entry| entry.expect("directory entry reads").path())
        .filter_map(|path| {
            (path.extension()? == "json").then(|| path.file_stem()?.to_str().map(str::to_owned))?
        })
        .collect();
    verbs.sort();
    assert!(
        !verbs.is_empty(),
        "{} holds no schema artifacts — the listing this drives is vacuous",
        dir.display()
    );
    verbs
}

/// `verb`'s committed artifact. Panics when it is missing or unparseable:
/// either is a broken pin, not a reason to check nothing.
pub fn committed_schema(verb: &str) -> Value {
    let path = schema_dir().join(format!("{verb}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} is unreadable: {e}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// The validator
// ---------------------------------------------------------------------------

/// Evidence that the walk did work. A validator whose traversal breaks reports
/// no errors, which is indistinguishable from a clean document unless the walk
/// itself is asserted.
#[derive(Default, Debug)]
pub struct Walk {
    pub keywords_applied: usize,
    pub refs_resolved: usize,
    pub properties_checked: usize,
    pub branches_taken: usize,
}

impl Walk {
    fn absorb(&mut self, other: &Walk) {
        self.keywords_applied += other.keywords_applied;
        self.refs_resolved += other.refs_resolved;
        self.properties_checked += other.properties_checked;
        self.branches_taken += other.branches_taken;
    }
}

/// Validate `instance` against `schema`, which is also the `$ref` resolution
/// root.
pub fn conform(instance: &Value, schema: &Value) -> (Vec<String>, Walk) {
    let mut walk = Walk::default();
    let errors = validate(instance, schema, schema, "", &mut walk);
    (errors, walk)
}

pub fn validate(
    instance: &Value,
    schema: &Value,
    root: &Value,
    at: &str,
    walk: &mut Walk,
) -> Vec<String> {
    let Some(obj) = schema.as_object() else {
        return vec![format!("{at}: schema is not an object: {schema}")];
    };

    let mut errors = Vec::new();
    for (keyword, argument) in obj {
        if ANNOTATIONS.contains(&keyword.as_str()) {
            continue;
        }
        walk.keywords_applied += 1;
        errors.extend(apply_keyword(keyword, argument, instance, root, at, walk));
    }
    errors.extend(closed_object_errors(obj, instance, at));
    errors
}

fn type_matches(instance: &Value, name: &str) -> Option<bool> {
    Some(match name {
        "object" => instance.is_object(),
        "array" => instance.is_array(),
        "string" => instance.is_string(),
        "boolean" => instance.is_boolean(),
        "null" => instance.is_null(),
        "integer" => instance.is_i64() || instance.is_u64(),
        "number" => instance.is_number(),
        _ => return None,
    })
}

/// The numeric widths schemars renders as `format`, and what each one rules
/// out beyond its `type`. Any other value is reported: an unchecked `format`
/// is a constraint the artifact states and this does not enforce.
fn format_errors(name: &str, instance: &Value, at: &str) -> Vec<String> {
    let unsigned = matches!(
        name,
        "uint" | "uint8" | "uint16" | "uint32" | "uint64" | "uint128"
    );
    let signed = matches!(
        name,
        "int" | "int8" | "int16" | "int32" | "int64" | "int128"
    );
    let real = matches!(name, "float" | "double");
    if !unsigned && !signed && !real {
        return vec![format!(
            "{at}: unsupported `format` value `{name}` — it constrains the artifact and this \
             validator would silently ignore it"
        )];
    }
    if !instance.is_number() {
        return Vec::new();
    }
    if real {
        return Vec::new();
    }
    if unsigned && !instance.is_u64() {
        return vec![format!(
            "{at}: `format: {name}` requires a non-negative integer, found {instance}"
        )];
    }
    if signed && !(instance.is_i64() || instance.is_u64()) {
        return vec![format!(
            "{at}: `format: {name}` requires an integer, found {instance}"
        )];
    }
    Vec::new()
}

fn apply_keyword(
    keyword: &str,
    argument: &Value,
    instance: &Value,
    root: &Value,
    at: &str,
    walk: &mut Walk,
) -> Vec<String> {
    let mut errors = Vec::new();
    match keyword {
        "$ref" => {
            let target = argument
                .as_str()
                .and_then(|r| r.strip_prefix("#/definitions/"))
                .and_then(|name| root.get("definitions")?.get(name));
            match target {
                Some(target) => {
                    walk.refs_resolved += 1;
                    let hop = format!("{at} -> {argument}");
                    errors.extend(validate(instance, target, root, &hop, walk));
                }
                None => errors.push(format!("{at}: unresolvable $ref {argument}")),
            }
        }
        "type" => {
            let names: Vec<&str> = match argument {
                Value::String(s) => vec![s.as_str()],
                Value::Array(items) => items.iter().filter_map(|i| i.as_str()).collect(),
                _ => {
                    errors.push(format!("{at}: `type` is neither string nor array"));
                    Vec::new()
                }
            };
            let mut matched = false;
            for name in &names {
                match type_matches(instance, name) {
                    Some(true) => matched = true,
                    Some(false) => {}
                    None => errors.push(format!("{at}: unsupported `type` value `{name}`")),
                }
            }
            if !names.is_empty() && !matched {
                errors.push(format!(
                    "{at}: expected type {} but found {}",
                    names.join("|"),
                    describe(instance)
                ));
            }
        }
        "format" => match argument.as_str() {
            Some(name) => errors.extend(format_errors(name, instance, at)),
            None => errors.push(format!("{at}: `format` is not a string: {argument}")),
        },
        "minimum" => match argument.as_f64() {
            Some(bound) => {
                if let Some(value) = instance.as_f64() {
                    if value < bound {
                        errors.push(format!("{at}: value {instance} is below minimum {argument}"));
                    }
                }
            }
            None => errors.push(format!("{at}: `minimum` is not a number: {argument}")),
        },
        "enum" => {
            let allowed = argument.as_array().cloned().unwrap_or_default();
            if !allowed.iter().any(|a| a == instance) {
                errors.push(format!("{at}: value {instance} is not one of {argument}"));
            }
        }
        "required" => {
            for name in argument.as_array().cloned().unwrap_or_default() {
                let Some(name) = name.as_str() else { continue };
                let present = instance.as_object().is_some_and(|o| o.contains_key(name));
                if !present {
                    errors.push(format!("{at}: required property `{name}` is missing"));
                }
            }
        }
        "properties" => {
            let Some(instance) = instance.as_object() else {
                return errors;
            };
            for (name, subschema) in argument.as_object().cloned().unwrap_or_default() {
                let Some(value) = instance.get(&name) else {
                    continue;
                };
                walk.properties_checked += 1;
                errors.extend(validate(
                    value,
                    &subschema,
                    root,
                    &format!("{at}/{name}"),
                    walk,
                ));
            }
        }
        "additionalProperties" => {
            if argument != &Value::Bool(false) {
                errors.push(format!(
                    "{at}: unsupported `additionalProperties` value {argument} \
                     — only `false` is implemented"
                ));
            }
        }
        "items" => match argument {
            Value::Object(_) => {
                for (i, element) in instance
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .enumerate()
                {
                    errors.extend(validate(
                        element,
                        argument,
                        root,
                        &format!("{at}[{i}]"),
                        walk,
                    ));
                }
            }
            _ => errors.push(format!(
                "{at}: unsupported tuple-form `items` — only a single subschema is implemented"
            )),
        },
        "oneOf" | "anyOf" | "allOf" => {
            let branches = argument.as_array().cloned().unwrap_or_default();
            let mut matching = Vec::new();
            let mut failures = Vec::new();
            for (i, branch) in branches.iter().enumerate() {
                let mut probe = Walk::default();
                let branch_errors = validate(
                    instance,
                    branch,
                    root,
                    &format!("{at}#{keyword}[{i}]"),
                    &mut probe,
                );
                if branch_errors.is_empty() {
                    matching.push((i, probe));
                } else {
                    failures.extend(branch_errors);
                }
            }
            let satisfied = match keyword {
                "oneOf" => matching.len() == 1,
                "anyOf" => !matching.is_empty(),
                _ => matching.len() == branches.len(),
            };
            if satisfied {
                walk.branches_taken += matching.len();
                for (_, probe) in &matching {
                    walk.absorb(probe);
                }
            } else {
                errors.push(format!(
                    "{at}: `{keyword}` unsatisfied — {} of {} branches matched",
                    matching.len(),
                    branches.len()
                ));
                errors.extend(failures);
            }
        }
        other => errors.push(format!(
            "{at}: unsupported schema keyword `{other}` — it constrains the artifact and this \
             validator would silently ignore it"
        )),
    }
    errors
}

/// Undeclared keys, which draft-07 would let through. See this file's header
/// for why they are rejected here.
fn closed_object_errors(schema: &Map<String, Value>, instance: &Value, at: &str) -> Vec<String> {
    let declares_shape =
        schema.contains_key("properties") || schema.contains_key("additionalProperties");
    let Some(instance) = instance.as_object() else {
        return Vec::new();
    };
    if !declares_shape {
        return Vec::new();
    }
    let declared = schema.get("properties").and_then(|p| p.as_object());
    instance
        .keys()
        .filter(|key| !declared.is_some_and(|d| d.contains_key(key.as_str())))
        .map(|key| format!("{at}: undeclared property `{key}`"))
        .collect()
}

fn describe(instance: &Value) -> &'static str {
    match instance {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// ---------------------------------------------------------------------------
// Guarding the guard
// ---------------------------------------------------------------------------

/// Every keyword an artifact uses, and the ones outside [`IMPLEMENTED`].
///
/// `seen` is the non-vacuity evidence: a walk that reads nothing reports no
/// unknown keywords, which is what a clean artifact also reports.
#[derive(Debug)]
pub struct Census {
    pub seen: usize,
    pub unknown: Vec<String>,
}

// ---------------------------------------------------------------------------
// Is the corpus wide enough for a pass to mean anything
// ---------------------------------------------------------------------------
//
// A document validating against a schema says nothing about the shapes the
// document never took. The artifact already lists them: every kebab-case
// variant tag and every closed vocabulary reaches the wire as a string `enum`
// on some property. Deriving the expectation from the artifact keeps the
// completeness question off a list someone has to remember to extend.
//
// Two shapes are out of range, and a corpus can omit them silently: a string
// enum reached through `items` (the members sit inside an array, not under
// the declaring key), and any non-string enum member.

/// Every `(property, member)` pair the artifact permits, `$ref`s resolved.
pub fn declared_enum_values(schema: &Value) -> BTreeSet<(String, String)> {
    let mut declared = BTreeSet::new();
    let mut stack = vec![schema.clone()];
    while let Some(node) = stack.pop() {
        match node {
            Value::Object(map) => {
                if let Some(props) = map.get("properties").and_then(|p| p.as_object()) {
                    for (name, sub) in props {
                        for member in enum_members(sub, schema, 0) {
                            declared.insert((name.clone(), member));
                        }
                    }
                }
                for (_, value) in map {
                    stack.push(value);
                }
            }
            Value::Array(items) => stack.extend(items),
            _ => {}
        }
    }
    declared
}

/// The string enum members `sub` permits directly, through a `$ref`, or
/// through a combinator. `items` is deliberately not followed.
fn enum_members(sub: &Value, root: &Value, depth: usize) -> Vec<String> {
    if depth > 8 {
        return Vec::new();
    }
    let mut members = Vec::new();
    let Some(map) = sub.as_object() else {
        return members;
    };
    if let Some(values) = map.get("enum").and_then(|e| e.as_array()) {
        members.extend(values.iter().filter_map(|v| v.as_str().map(str::to_owned)));
    }
    if let Some(target) = map
        .get("$ref")
        .and_then(|r| r.as_str())
        .and_then(|r| r.strip_prefix("#/definitions/"))
        .and_then(|name| root.get("definitions")?.get(name))
    {
        members.extend(enum_members(target, root, depth + 1));
    }
    for combinator in ["allOf", "anyOf", "oneOf"] {
        for branch in map
            .get(combinator)
            .and_then(|b| b.as_array())
            .cloned()
            .unwrap_or_default()
        {
            members.extend(enum_members(&branch, root, depth + 1));
        }
    }
    members
}

/// Every `(key, value)` pair `doc` carries where the value is a string.
pub fn observed_enum_values(doc: &Value) -> BTreeSet<(String, String)> {
    let mut observed = BTreeSet::new();
    let mut stack = vec![doc.clone()];
    while let Some(node) = stack.pop() {
        match node {
            Value::Object(map) => {
                for (key, value) in map {
                    if let Some(text) = value.as_str() {
                        observed.insert((key.clone(), text.to_owned()));
                    }
                    stack.push(value);
                }
            }
            Value::Array(items) => stack.extend(items),
            _ => {}
        }
    }
    observed
}

pub fn census(schema: &Value) -> Census {
    let mut seen = 0usize;
    let mut unknown = Vec::new();
    let mut stack = vec![(schema.clone(), String::new(), false)];
    while let Some((node, at, is_schema_map)) = stack.pop() {
        match node {
            Value::Object(map) => {
                for (key, value) in map {
                    let child = format!("{at}/{key}");
                    if is_schema_map {
                        stack.push((value, child, false));
                        continue;
                    }
                    seen += 1;
                    if !IMPLEMENTED.contains(&key.as_str()) && !ANNOTATIONS.contains(&key.as_str())
                    {
                        unknown.push(child.clone());
                    }
                    let holds_schema_map = key == "properties" || key == "definitions";
                    stack.push((value, child, holds_schema_map));
                }
            }
            Value::Array(items) => {
                for (i, item) in items.into_iter().enumerate() {
                    stack.push((item, format!("{at}[{i}]"), false));
                }
            }
            _ => {}
        }
    }
    Census { seen, unknown }
}
