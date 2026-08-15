//! The bytes `rwv doctor --json` emits must satisfy the schema operators are
//! told to read them with — `docs/reference/schemas/doctor.json`.
//!
//! The regression this exists for: the envelope was described in two places
//! that were never compared. The committed schema was derived from a struct
//! nothing serialized, while the bytes on the wire came from an unrelated
//! hand-written `serde_json::json!` literal. Both were plausible; a divergence
//! between them was unobservable. One shared type removes the fork, and this
//! file is what notices a re-fork, because it reads the committed artifact
//! rather than asserting which type the generator happens to point at.
//!
//! The validator lives in `tests/common/json_schema.rs`, which documents the
//! draft-07 subset it implements and the two places it is deliberately
//! stricter. It is shared because every `--json` verb needs this question
//! asked, and a second validator is the same fork one level up.
//!
//! # Residue
//!
//!   - The corpus here samples the envelope's neighbours rather than every
//!     `ViolationOutput` variant. Every-variant coverage is
//!     `tests/schema_conformance_test.rs`, which drives the same envelope from
//!     `tests/common/doctor_corpus.rs`; `tests/doctor_render_parity_test.rs`
//!     is what pins every variant reaching `--json` at all.
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
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

mod common;

use common::json_schema::{self, Walk};

fn schema() -> Value {
    json_schema::committed_schema("doctor")
}

fn schema_path() -> String {
    json_schema::schema_path("doctor")
}

fn check(instance: &Value) -> (Vec<String>, Walk) {
    json_schema::conform(instance, &schema())
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
        workspace: "/ws".to_owned(),
        workweave: Some("proj--feat-a".into()),
        workweave_unregistered: false,
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
    advisories: Vec<repoweave::workspace::AdvisoryOutput>,
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
        advisories,
    ))
    .expect("doctor payload serializes")
}

/// One sample of the advisory vocabulary doctor shares with `rwv sync --json`.
fn advisories() -> Vec<repoweave::workspace::AdvisoryOutput> {
    vec![repoweave::workspace::AdvisoryOutput {
        kind: repoweave::workspace::AdvisoryKindOutput::DerivedStateStale,
        remedy: "rwv materialize".to_owned(),
        inputs: vec!["projects/proj/rwv.lock".to_owned()],
    }]
}

fn populated() -> Value {
    emit(
        corpus(),
        issues(),
        Some(resolution()),
        plugins(),
        advisories(),
    )
}

// ---------------------------------------------------------------------------
// The pin
// ---------------------------------------------------------------------------

#[test]
fn emitted_output_validates_against_the_committed_schema() {
    let (errors, walk) = check(&populated());
    assert!(
        errors.is_empty(),
        "`rwv doctor --json` output does not satisfy {}:\n  {}",
        schema_path(),
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
    let (errors, walk) = check(&emit(Vec::new(), Vec::new(), None, Vec::new(), Vec::new()));
    assert!(
        errors.is_empty(),
        "clean-workspace output does not satisfy {}:\n  {}",
        schema_path(),
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
        url.ends_with(&format!("/{}", schema_path())),
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
    let errors = json_schema::validate(&json!("bbb"), &schema, &schema, "", &mut walk);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("unsupported schema keyword `pattern`")),
        "an unimplemented keyword must be reported, got {errors:?}"
    );
}
