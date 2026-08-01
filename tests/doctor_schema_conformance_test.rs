//! The bytes `rwv doctor --json` emits must satisfy the schema operators are
//! told to read them with — `docs/reference/schemas/doctor.json`, embedded
//! here at compile time.
//!
//! The regression this exists for: the envelope was described in two places
//! that were never compared. The committed schema was derived from a struct
//! nothing serialized, while the bytes on the wire came from an unrelated
//! hand-written `serde_json::json!` literal. Both were plausible; a divergence
//! between them was unobservable. One shared type removes the fork, and this
//! file is what notices a re-fork, because it reads the committed artifact
//! rather than asserting which type the generator happens to point at.
//!
//! # What the validator covers, and what it refuses to
//!
//! It implements the draft-07 keyword subset schemars actually emits:
//! `$ref`, `type`, `enum`, `required`, `properties`, `additionalProperties`,
//! `items`, `oneOf`, `anyOf`, `allOf`. Every other keyword is **reported as an
//! error**, not ignored. A validator that skips what it does not understand
//! green-lights the part it looked at and reads as green over the part it did
//! not — so if schemars ever starts emitting `format`, `pattern` or
//! `minItems` here, this stops the tree until someone implements it.
//!
//! Two deliberate departures from draft-07 semantics, both tightening:
//!
//!   1. An object schema is treated as closed whenever it declares
//!      `properties` or `additionalProperties`. Draft-07 would allow undeclared
//!      keys through, and an added key is precisely the shape a re-forked
//!      hand-minted envelope takes. Every object in this schema comes from a
//!      Rust struct or enum variant, so nothing legitimate is rejected: a new
//!      field moves the schema artifact and the artifact is what this reads.
//!   2. A boolean schema (`true` / `false` in schema position) is an error
//!      rather than always-pass / always-fail. schemars does not emit them.
//!
//! # Residue
//!
//!   - Coverage of `ViolationOutput` variants is only as wide as [`corpus`],
//!     which samples the envelope's neighbours rather than every variant.
//!     `tests/doctor_render_parity_test.rs` is what pins every variant
//!     reaching `--json` at all; this file pins the shape of what comes out.
//!   - Coverage of the `issues` array is two entries: one fieldless kind and
//!     the one carrying an observation, which is what exercises both `kind`
//!     shapes and the `MemberIncompatibilityOutput` definition. Every other
//!     kind serializes through the same fieldless arm.
//!   - Validating against the committed artifact says nothing about whether
//!     that artifact is current. The generator drift gate in
//!     `scripts/ci-local.sh` is what pins that, and both are needed: this test
//!     alone passes against a stale artifact the runtime still happens to fit.

use repoweave::check::{
    build_doctor_json, CheckViolation, DriftKind, IndexDriftKind, WeaveRootIdentityConflictKind,
    WorkingTreeDriftKind, DOCTOR_SCHEMA_URL,
};
use repoweave::integration::{Issue, IssueKind, Severity};
use repoweave::integrations::merge::MemberIncompatibility;
use repoweave::manifest::{ProjectName, RepoPath, WorkweaveName};
use repoweave::plugins::PluginRecord;
use repoweave::vcs::ResolvedRevisionId;
use repoweave::workspace::Resolution;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const COMMITTED_SCHEMA_PATH: &str = "docs/reference/schemas/doctor.json";
const COMMITTED_SCHEMA: &str = include_str!("../docs/reference/schemas/doctor.json");

fn schema() -> Value {
    serde_json::from_str(COMMITTED_SCHEMA).expect("committed doctor schema is valid JSON")
}

// ---------------------------------------------------------------------------
// A draft-07 validator over the keyword subset schemars emits
// ---------------------------------------------------------------------------

/// Keywords that constrain nothing. Anything absent from both this list and
/// the match in [`apply_keyword`] is reported rather than skipped.
const ANNOTATIONS: &[&str] = &[
    "$schema",
    "$id",
    "title",
    "description",
    "default",
    "examples",
    "definitions",
];

/// Evidence that the walk did work. A validator whose traversal breaks reports
/// no errors, which is indistinguishable from a clean document unless the walk
/// itself is asserted.
#[derive(Default, Debug)]
struct Walk {
    keywords_applied: usize,
    refs_resolved: usize,
    properties_checked: usize,
    branches_taken: usize,
}

impl Walk {
    fn absorb(&mut self, other: &Walk) {
        self.keywords_applied += other.keywords_applied;
        self.refs_resolved += other.refs_resolved;
        self.properties_checked += other.properties_checked;
        self.branches_taken += other.branches_taken;
    }
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

fn validate(
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

fn check(instance: &Value) -> (Vec<String>, Walk) {
    let schema = schema();
    let mut walk = Walk::default();
    let errors = validate(instance, &schema, &schema, "", &mut walk);
    (errors, walk)
}

// ---------------------------------------------------------------------------
// Emitting real doctor output
// ---------------------------------------------------------------------------

fn project() -> ProjectName {
    ProjectName::new("proj").unwrap()
}

fn repo() -> RepoPath {
    RepoPath::new("github/acme/repo").unwrap()
}

fn workweave() -> WorkweaveName {
    WorkweaveName::new("feat-a").unwrap()
}

/// Samples chosen for the schema constructs they exercise, not for variant
/// coverage: a bare struct variant, one carrying an optional field at both
/// settings, and two sub-kind enums (`$ref` into a nested `oneOf`).
fn corpus() -> Vec<CheckViolation> {
    vec![
        CheckViolation::OrphanedClone { path: repo() },
        CheckViolation::StaleLock {
            project: project(),
            repo: repo(),
            locked: ResolvedRevisionId::from_canonical("a".repeat(40), None),
            actual: ResolvedRevisionId::from_canonical("b".repeat(40), None),
        },
        CheckViolation::WorkweaveDrift {
            workweave: workweave(),
            kind: DriftKind::Missing,
            repo: repo(),
        },
        CheckViolation::IndexDrift {
            workweave: Some(workweave()),
            repo: repo(),
            kind: IndexDriftKind::SafeToFix,
        },
        CheckViolation::WorkingTreeDrift {
            workweave: None,
            repo: repo(),
            kind: WorkingTreeDriftKind::LiveEdits,
        },
        CheckViolation::WeaveRootIdentityConflict {
            root: PathBuf::from("/ws"),
            pointer_project: Some(project()),
            sub_kind: WeaveRootIdentityConflictKind::RegisteredWorkweave {
                project: "proj".into(),
                workweave_name: "feat-a".into(),
            },
        },
        CheckViolation::UnparseableProject {
            project: project(),
            manifest_path: PathBuf::from("/ws/projects/proj/rwv.toml"),
            message: "bad yaml".into(),
        },
    ]
}

fn plugins() -> Vec<PluginRecord> {
    vec![
        PluginRecord {
            name: "demo".into(),
            path: "/usr/local/bin/rwv-demo".into(),
            shadowed: false,
            shadowed_by: None,
        },
        PluginRecord {
            name: "demo".into(),
            path: "/opt/bin/rwv-demo".into(),
            shadowed: true,
            shadowed_by: Some("/usr/local/bin/rwv-demo".into()),
        },
    ]
}

fn resolution() -> Resolution {
    Resolution {
        workspace: PathBuf::from("/ws"),
        workweave: Some("proj--feat-a".into()),
        project: "proj".into(),
    }
}

/// Both shapes an `issues` entry's `kind` can take: a fieldless kind, which
/// serializes as a plain string, and the one carrying an observation, which
/// serializes as a single-key object. A corpus of only the first would leave
/// `MemberIncompatibilityOutput` in the schema with nothing validating against
/// it.
fn issues() -> Vec<Issue> {
    vec![
        Issue {
            integration: "static-files".into(),
            severity: Severity::Warning,
            message: "projects/proj/CLAUDE.md is declared but not surfaced".into(),
            kind: IssueKind::Surfacing,
            safe_to_fix: true,
        },
        Issue {
            integration: "go-work".into(),
            severity: Severity::Error,
            message: "member-incompatibility: go.work sets `go` to `1.21`".into(),
            kind: IssueKind::MemberIncompatibility(Box::new(MemberIncompatibility::new(
                "go-work",
                Path::new("/ws/projects/proj/go.work"),
                "go",
                "1.21",
                "1.23",
                "github/acme/repo/go.mod",
            ))),
            safe_to_fix: false,
        },
    ]
}

fn emit(
    violations: Vec<CheckViolation>,
    issues: Vec<Issue>,
    res: Option<Resolution>,
    plugins: Vec<PluginRecord>,
) -> Value {
    let mut workweave_dirs = HashMap::new();
    workweave_dirs.insert(workweave(), PathBuf::from("/ws/.workweaves/proj--feat-a"));
    serde_json::to_value(build_doctor_json(
        violations,
        issues,
        Path::new("/ws"),
        &workweave_dirs,
        res,
        plugins,
    ))
    .expect("doctor payload serializes")
}

fn populated() -> Value {
    emit(corpus(), issues(), Some(resolution()), plugins())
}

// ---------------------------------------------------------------------------
// The pin
// ---------------------------------------------------------------------------

#[test]
fn emitted_output_validates_against_the_committed_schema() {
    let (errors, walk) = check(&populated());
    assert!(
        errors.is_empty(),
        "`rwv doctor --json` output does not satisfy {COMMITTED_SCHEMA_PATH}:\n  {}",
        errors.join("\n  ")
    );

    // Non-vacuity: a traversal that quietly stopped early reports the same
    // empty error list as a clean document.
    assert!(
        walk.refs_resolved >= corpus().len(),
        "expected at least one $ref hop per violation, walked {walk:?}"
    );
    assert!(
        walk.properties_checked > 30,
        "the walk visited too few properties to have covered the envelope: {walk:?}"
    );
    assert!(
        walk.branches_taken >= corpus().len(),
        "expected one oneOf branch per violation, walked {walk:?}"
    );
}

#[test]
fn empty_envelope_validates_against_the_committed_schema() {
    let (errors, walk) = check(&emit(Vec::new(), Vec::new(), None, Vec::new()));
    assert!(
        errors.is_empty(),
        "clean-workspace output does not satisfy {COMMITTED_SCHEMA_PATH}:\n  {}",
        errors.join("\n  ")
    );
    assert!(
        walk.properties_checked >= 3,
        "the walk never reached the envelope's own properties: {walk:?}"
    );
}

#[test]
fn schema_url_on_the_wire_names_the_artifact_validated_here() {
    let doc = populated();
    let url = doc["$schema"].as_str().expect("$schema is a string");
    assert_eq!(url, DOCTOR_SCHEMA_URL);
    assert!(
        url.ends_with(&format!("/{COMMITTED_SCHEMA_PATH}")),
        "the emitted $schema URL points somewhere other than the artifact this test validates \
         against; got {url}"
    );
}

// ---------------------------------------------------------------------------
// Seeded failures
// ---------------------------------------------------------------------------

/// The divergence the unified type exists to prevent, re-created by hand: the
/// envelope minted independently of the schema, with one plausible drift each.
///
/// Every one of these validated as an envelope before — nothing compared them
/// to the artifact operators read.
#[test]
fn a_re_forked_envelope_is_reported() {
    let outputs = || populated()["violations"].clone();

    let renamed_key = json!({
        "$schema": DOCTOR_SCHEMA_URL,
        "findings": outputs(),
        "plugins": [],
    });
    let extra_key = json!({
        "$schema": DOCTOR_SCHEMA_URL,
        "violations": outputs(),
        "plugins": [],
        "scope": "all",
    });
    let dropped_key = json!({
        "$schema": DOCTOR_SCHEMA_URL,
        "violations": outputs(),
    });
    let retyped_key = json!({
        "$schema": DOCTOR_SCHEMA_URL,
        "violations": { "count": 1 },
        "plugins": [],
    });

    for (name, doc, expected) in [
        (
            "violations renamed to findings",
            renamed_key,
            "`violations`",
        ),
        ("an added top-level key", extra_key, "`scope`"),
        ("plugins dropped", dropped_key, "`plugins`"),
        (
            "violations turned into an object",
            retyped_key,
            "expected type",
        ),
    ] {
        let (errors, _) = check(&doc);
        assert!(
            errors.iter().any(|e| e.contains(expected)),
            "{name}: expected a report mentioning {expected}, got {errors:?}"
        );
    }
}

#[test]
fn a_drifted_violation_is_reported() {
    let mut doc = populated();
    doc["violations"][0]
        .as_object_mut()
        .unwrap()
        .remove("absolute_path");
    let (errors, _) = check(&doc);
    assert!(
        errors.iter().any(|e| e.contains("oneOf")),
        "dropping a violation field should leave no matching variant, got {errors:?}"
    );

    let mut doc = populated();
    doc["violations"][0]["kind"] = json!("invented-kind");
    let (errors, _) = check(&doc);
    assert!(
        errors.iter().any(|e| e.contains("oneOf")),
        "an unknown `kind` should match no variant, got {errors:?}"
    );
}

#[test]
fn a_drifted_leaf_type_is_reported() {
    let mut doc = populated();
    doc["plugins"][0]["shadowed"] = json!("yes");
    let (errors, _) = check(&doc);
    assert!(
        errors.iter().any(|e| e.contains("expected type boolean")),
        "a stringified boolean should be reported, got {errors:?}"
    );

    let mut doc = populated();
    doc["resolution"]["workspace"] = json!(5);
    let (errors, _) = check(&doc);
    assert!(
        errors.iter().any(|e| e.contains("expected type")),
        "a numeric workspace path should be reported, got {errors:?}"
    );
}

/// Guards the guard: an unimplemented keyword must stop the tree rather than
/// pass silently, or every later addition to the schema goes unchecked.
#[test]
fn an_unimplemented_keyword_is_reported() {
    let schema = json!({ "type": "string", "pattern": "^a+$" });
    let mut walk = Walk::default();
    let errors = validate(&json!("bbb"), &schema, &schema, "", &mut walk);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("unsupported schema keyword `pattern`")),
        "an unimplemented keyword must be reported, got {errors:?}"
    );
}

/// The committed artifact must stay inside the subset above. This is the check
/// that turns "schemars emitted something new" into a failure here rather than
/// into silent under-validation of the real output.
#[test]
fn the_committed_schema_uses_no_keyword_the_validator_ignores() {
    let implemented = [
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
    ];
    let mut seen = 0usize;
    let mut unknown = Vec::new();
    let mut stack = vec![(schema(), String::new(), false)];
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
                    if !implemented.contains(&key.as_str()) && !ANNOTATIONS.contains(&key.as_str())
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
    assert!(seen > 100, "the keyword walk read almost nothing: {seen}");
    assert!(
        unknown.is_empty(),
        "{COMMITTED_SCHEMA_PATH} uses keywords this validator does not implement, so the \
         emitted output is only partly checked: {unknown:?}"
    );
}
