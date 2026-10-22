//! Every committed artifact under `docs/reference/schemas/` must have output
//! validated against it, and the registry that says which ones do is the
//! directory listing rather than a list someone maintains.
//!
//! `--json` is advertised as the agent-integration surface, each envelope
//! embeds a `$schema` URL naming one of these files, and the files are
//! regenerated from the Rust types by `cargo run --bin generate-explain`.
//! Regeneration proves the artifact matches the type. It does not prove a
//! serialized envelope satisfies the artifact: a `skip_serializing_if` that
//! drops a required key, a `flatten` that moves one, a rename on one side
//! only, all survive regeneration. This file is where that is asked, of every
//! verb, from one table.
//!
//! Documents here are built by serializing the production envelope types. What
//! a verb writes to stdout is a separate question with a separate answer in
//! `tests/schema_conformance_wire_test.rs`: an envelope this file blesses can
//! still reach nobody if the print path mints its own bytes.
//!
//! The validator, and the draft-07 subset it implements, is
//! `tests/common/json_schema.rs`.
//!
//! # Adding a field to an envelope
//!
//! Six of the seven envelopes are built here with struct literals, so a new
//! field is a compile error naming the file and line. That is the intended
//! cost: the alternative is a corpus that silently never samples the new field,
//! which validates and proves nothing about it. Add the field to the sample,
//! and to the second sample if the field is optional — an `Option` wants both
//! settings, since `skip_serializing_if` makes them different wire shapes.
//!
//! **`doctor` is the exception, and it is the gap in that property.** Its
//! report renderer takes only what the findings pipeline builds, so its
//! documents come from `common::doctor_corpus::wire_document`, which writes the
//! `DoctorJsonOutput` literal itself and mints each record through the
//! production `ViolationOutput::from_violation`. A new field on
//! `ViolationOutput` leaves this file compiling, sampled at whatever that
//! renderer yields from the inputs below, which for an `Option` is plausibly
//! the absent shape and nothing else. Minting the records in production code
//! is what makes them real emissions rather than a test's guess at them, so
//! the exchange is deliberate; it is not free.
//!
//! [`every_corpus_carries_every_envelope_field_its_schema_declares`] narrows
//! that exception to nested fields: a new *envelope* field on any verb,
//! doctor included, cannot reach the regenerated artifact and stay unsampled.
//! A new field on a nested type doctor mints inside the builder still can.
//!
//! Two neighboring shapes do *not* widen the gap, both checked rather than
//! assumed. A new *variant* is caught by
//! [`every_corpus_exercises_every_member_its_schema_declares`], which reads the
//! regenerated artifact. And no envelope reaches a type through
//! `#[serde(flatten)]`: every flatten in `src/` sits on an NDJSON record
//! (`fetch.rs`, `push.rs`, `update.rs`, `sync.rs`). The `allOf` in
//! `docs/reference/schemas/update.json` and `doctor.json` is schemars wrapping
//! a single `$ref` beside a description, not a flattened struct. Per-record
//! NDJSON artifacts (`fetch-record.json`, `push-record.json`, etc.) are
//! committed alongside their envelope counterparts; the corpus here covers the
//! envelopes only. Wire conformance of the NDJSON records is in
//! `tests/schema_conformance_wire_test.rs`.
//!
//! # Residue
//!
//!   - `doctor` is registered here for envelope conformance and for
//!     `ViolationOutput` variant width, taken from `tests/common/doctor_corpus.rs`.
//!     `tests/doctor_schema_conformance_test.rs` is the doctor-specific
//!     instrument and holds the seeded-divergence corpus for that envelope.
//!   - Enum-member coverage is a floor, not a proof of variant completeness:
//!     it reads members the artifact declares under a property name, so a
//!     member reachable only through an array's `items` is not counted.

mod common;

use common::json_schema;
use repoweave::check::{CheckViolation, DOCTOR_SCHEMA_URL};
use repoweave::fetch::{
    FetchJsonOutput, FetchOutcomeNdjsonRecord, FetchOutcomeOutput, FetchOutcomeStatus,
    FETCH_RECORD_SCHEMA_URL, FETCH_SCHEMA_URL,
};
use repoweave::integration::{Issue, IssueKind, IssueSource, Severity};
use repoweave::integration::{MemberIncompatibility, MissingTool, Observed};
use repoweave::integrations::GoWork;
use repoweave::integrations::NpmWorkspaces;
use repoweave::op_state::{OpPhase, OpVerb, Override};
use repoweave::path_spelling::WirePath;
use repoweave::placement::{PlacementNdjsonRecord, PlacementOutput, PLACEMENT_RECORD_SCHEMA_URL};
use repoweave::push::{
    PushJsonOutput, PushOutcomeNdjsonRecord, PushOutcomeOutput, PUSH_RECORD_SCHEMA_URL,
    PUSH_SCHEMA_URL,
};
use repoweave::share::FilePlacement;
use repoweave::skipped::{SkipReason, SkippedEffect};
use repoweave::status::{
    LockRelation, OpStatus, ParentInfo, RepoStatus, StatusJsonOutput, STATUS_SCHEMA_URL,
};
use repoweave::sync::{
    AdvanceNdjsonRecord, AdvanceOutput, ContainmentOutput, ContainmentVerdictOutput,
    MemberNdjsonRecord, MemberOutput, ReplayBaseline, Step3AdvanceOutput, SyncFailureOutput,
    SyncJsonOutput, SyncOutcomeNdjsonRecord, SyncOutcomeOutput, SyncToJsonOutput,
    ADVANCE_RECORD_SCHEMA_URL, MEMBER_RECORD_SCHEMA_URL, SYNC_JSON_SCHEMA_URL,
    SYNC_RECORD_SCHEMA_URL, SYNC_TO_JSON_SCHEMA_URL, SYNC_TO_RECORD_SCHEMA_URL,
};
use repoweave::update::{
    RepoUpdateRecord, UpdateJsonOutput, UpdateKind, UpdateNdjsonRecord, UPDATE_RECORD_SCHEMA_URL,
    UPDATE_SCHEMA_URL,
};
use repoweave::vcs::{ConflictOp, MidOperation, VcsErrorOutput};
use repoweave::workspace::{AdvisoryKindOutput, AdvisoryOutput, Resolution};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

// The bundled rwv-each plugin's wire types: a binary's items are not linkable
// from here, so its source is compiled in.
#[path = "../src/bin/rwv-each/schema_url.rs"]
mod each_schema_url;
#[allow(dead_code)]
#[path = "../src/bin/rwv-each/wire.rs"]
mod each_wire;

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

/// One verb's committed artifact and the emissions validated against it.
struct VerbCase {
    verb: &'static str,
    /// The URL the verb embeds. Pinned separately from the artifact it names:
    /// a consumer fetches what the bytes point at, not what a test knows.
    schema_url: &'static str,
    corpus: fn() -> Vec<(&'static str, Value)>,
}

fn cases() -> Vec<VerbCase> {
    vec![
        VerbCase {
            verb: "doctor",
            schema_url: DOCTOR_SCHEMA_URL,
            corpus: doctor_corpus,
        },
        VerbCase {
            verb: "fetch",
            schema_url: FETCH_SCHEMA_URL,
            corpus: fetch_corpus,
        },
        VerbCase {
            verb: "push",
            schema_url: PUSH_SCHEMA_URL,
            corpus: push_corpus,
        },
        VerbCase {
            verb: "status",
            schema_url: STATUS_SCHEMA_URL,
            corpus: status_corpus,
        },
        VerbCase {
            verb: "sync",
            schema_url: SYNC_JSON_SCHEMA_URL,
            corpus: sync_corpus,
        },
        VerbCase {
            verb: "sync-to",
            schema_url: SYNC_TO_JSON_SCHEMA_URL,
            corpus: sync_to_corpus,
        },
        VerbCase {
            verb: "update",
            schema_url: UPDATE_SCHEMA_URL,
            corpus: update_corpus,
        },
        VerbCase {
            verb: "fetch-record",
            schema_url: FETCH_RECORD_SCHEMA_URL,
            corpus: fetch_record_corpus,
        },
        VerbCase {
            verb: "push-record",
            schema_url: PUSH_RECORD_SCHEMA_URL,
            corpus: push_record_corpus,
        },
        VerbCase {
            verb: "update-record",
            schema_url: UPDATE_RECORD_SCHEMA_URL,
            corpus: update_record_corpus,
        },
        VerbCase {
            verb: "sync-record",
            schema_url: SYNC_RECORD_SCHEMA_URL,
            corpus: sync_record_corpus,
        },
        VerbCase {
            verb: "sync-to-record",
            schema_url: SYNC_TO_RECORD_SCHEMA_URL,
            corpus: sync_to_record_corpus,
        },
        VerbCase {
            verb: "placement-record",
            schema_url: PLACEMENT_RECORD_SCHEMA_URL,
            corpus: placement_record_corpus,
        },
        VerbCase {
            verb: "member-record",
            schema_url: MEMBER_RECORD_SCHEMA_URL,
            corpus: member_record_corpus,
        },
        VerbCase {
            verb: "advance-record",
            schema_url: ADVANCE_RECORD_SCHEMA_URL,
            corpus: advance_record_corpus,
        },
        VerbCase {
            verb: "each",
            schema_url: each_schema_url::EACH_SCHEMA_URL,
            corpus: each_corpus,
        },
        VerbCase {
            verb: "each-record",
            schema_url: each_schema_url::EACH_RECORD_SCHEMA_URL,
            corpus: each_record_corpus,
        },
    ]
}

// ---------------------------------------------------------------------------
// Shared sample values
// ---------------------------------------------------------------------------

const REPO: &str = "github/acme/repo";
const ABS: &str = "/ws/github/acme/repo";

fn wire(path: &str) -> WirePath {
    WirePath::of(Path::new(path))
}

fn sha(byte: char) -> String {
    std::iter::repeat_n(byte, 40).collect()
}

fn resolution() -> Resolution {
    Resolution {
        workspace: repoweave::path_spelling::WirePath::of(std::path::Path::new("/ws")),
        workweave: Some("proj--feat-a".into()),
        workweave_unregistered: false,
        project: repoweave::manifest::ProjectName::new("proj").unwrap(),
    }
}

fn primary_resolution() -> Resolution {
    Resolution {
        workspace: repoweave::path_spelling::WirePath::of(std::path::Path::new("/ws")),
        workweave: None,
        workweave_unregistered: false,
        project: repoweave::manifest::ProjectName::new("proj").unwrap(),
    }
}

/// The third checkout state: in a workweave, with no registry entry naming
/// it. Distinct from both above, and the one a present-or-absent `workweave`
/// cannot carry.
fn unregistered_resolution() -> Resolution {
    Resolution {
        workspace: repoweave::path_spelling::WirePath::of(std::path::Path::new("/ws")),
        workweave: None,
        workweave_unregistered: true,
        project: repoweave::manifest::ProjectName::new("proj").unwrap(),
    }
}

fn advance() -> Step3AdvanceOutput {
    Step3AdvanceOutput {
        from_sha: sha('a'),
        to_sha: sha('b'),
    }
}

/// A checkout in this workspace and one in another, as sync-to places both.
fn placements() -> Vec<PlacementOutput> {
    vec![
        PlacementOutput {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            files: FilePlacement {
                reflinked: 4,
                hardlinked: 2,
                written: 1,
            },
        },
        PlacementOutput {
            path: REPO.to_owned(),
            absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new(
                "/target/github/acme/repo",
            )),
            files: FilePlacement {
                reflinked: 0,
                hardlinked: 0,
                written: 3,
            },
        },
    ]
}

fn advisories() -> Vec<AdvisoryOutput> {
    vec![AdvisoryOutput {
        kind: AdvisoryKindOutput::DerivedStateStale,
        remedy: "rwv materialize".to_owned(),
        inputs: vec!["projects/proj/rwv.lock".to_owned()],
    }]
}

fn value(envelope: impl serde::Serialize) -> Value {
    serde_json::to_value(envelope).expect("envelope serializes")
}

// ---------------------------------------------------------------------------
// doctor
// ---------------------------------------------------------------------------

/// Two members `tests/common/doctor_corpus.rs` does not reach: it fixes every
/// sample's `severity` at `warning`, and samples the only violation carrying an
/// `OpVerb` at `sync-to`. Both are wire values with nothing else to produce
/// them, so they are supplied here rather than left unvalidated.
fn doctor_corpus_supplement() -> (Vec<CheckViolation>, Vec<Issue>) {
    let violations = vec![CheckViolation::StaleOpState {
        workspace_dir: common::doctor_corpus::path("/ws"),
        verb: OpVerb::Sync,
        started_at: "2026-01-01T00:00:00Z".to_owned(),
    }];
    let issues = vec![Issue {
        source: IssueSource::integration("go-work"),
        severity: Severity::Error,
        message: "member-incompatibility: go.work sets `go` to `1.21`"
            .to_owned()
            .into(),
        kind: IssueKind::MemberIncompatibility(Box::new(MemberIncompatibility::new(
            &GoWork,
            &common::doctor_corpus::path("/ws/projects/proj/go.work"),
            Observed {
                key: "go",
                on_disk: "1.21",
                required: "1.23",
                required_by: "github/acme/repo/go.mod",
            },
        ))),
        safe_to_fix: false,
    }];
    (violations, issues)
}

fn doctor_corpus() -> Vec<(&'static str, Value)> {
    let mut workweave_dirs = HashMap::new();
    workweave_dirs.insert(
        common::doctor_corpus::workweave(),
        common::doctor_corpus::path("/ws/.workweaves/proj--feat-a"),
    );
    let (extra_violations, extra_issues) = doctor_corpus_supplement();
    let mut violations = common::doctor_corpus::corpus();
    violations.extend(extra_violations);
    let mut issues = common::doctor_corpus::issue_corpus();
    issues.extend(extra_issues);
    let populated = serde_json::to_value(common::doctor_corpus::wire_document(
        violations,
        issues,
        &common::doctor_corpus::path("/ws"),
        &workweave_dirs,
        Some(resolution()),
        Vec::new(),
        advisories(),
    ))
    .expect("doctor payload serializes");
    let empty = serde_json::to_value(common::doctor_corpus::wire_document(
        Vec::new(),
        Vec::new(),
        &common::doctor_corpus::path("/ws"),
        &HashMap::new(),
        None,
        Vec::new(),
        Vec::new(),
    ))
    .expect("doctor payload serializes");
    vec![("every-violation-and-issue", populated), ("clean", empty)]
}

// ---------------------------------------------------------------------------
// fetch
// ---------------------------------------------------------------------------

/// Every [`FetchOutcomeStatus`], and `message` at both settings. The match is
/// exhaustive so a new status stops this file compiling.
fn fetch_outcomes() -> Vec<FetchOutcomeOutput> {
    [
        FetchOutcomeStatus::Ok,
        FetchOutcomeStatus::Skipped,
        FetchOutcomeStatus::Failed,
    ]
    .into_iter()
    .map(|status| {
        let message = match status {
            FetchOutcomeStatus::Ok => None,
            FetchOutcomeStatus::Skipped => Some("role: reference".to_owned()),
            FetchOutcomeStatus::Failed => Some("remote unreachable".to_owned()),
        };
        FetchOutcomeOutput {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            status,
            message,
        }
    })
    .collect()
}

/// One record of every shape a skipped side effect takes.
fn every_skip() -> Vec<SkippedEffect> {
    let mut skips = vec![
        SkippedEffect::install_hooks(SkipReason::NoMaterialize),
        SkippedEffect::install_hooks(SkipReason::InsideWorkweave),
        SkippedEffect::tool_missing(&NpmWorkspaces, MissingTool::of::<NpmWorkspaces>()),
        SkippedEffect::activation_while_active(
            &repoweave::manifest::ProjectName::new("other").unwrap(),
            &repoweave::manifest::ProjectName::new("app").unwrap(),
        ),
        SkippedEffect::hooks_while_active(&repoweave::manifest::ProjectName::new("app").unwrap()),
    ];
    skips.extend(SkippedEffect::not_presented(
        &repoweave::manifest::ProjectName::new("other").unwrap(),
    ));
    skips.extend(SkippedEffect::narrowed());
    skips
}

fn fetch_corpus() -> Vec<(&'static str, Value)> {
    vec![
        (
            "every-status",
            value(FetchJsonOutput {
                schema: FETCH_SCHEMA_URL.to_owned(),
                outcomes: fetch_outcomes(),
                placements: placements(),
                resolution: Some(resolution()),
                skipped: every_skip(),
            }),
        ),
        (
            "unresolved-workspace",
            value(FetchJsonOutput {
                schema: FETCH_SCHEMA_URL.to_owned(),
                outcomes: Vec::new(),
                placements: Vec::new(),
                resolution: None,
                skipped: Vec::new(),
            }),
        ),
    ]
}

// ---------------------------------------------------------------------------
// push
// ---------------------------------------------------------------------------

fn push_outcomes() -> Vec<PushOutcomeOutput> {
    vec![
        PushOutcomeOutput::Pushed {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
        },
        PushOutcomeOutput::Skipped {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
        },
        PushOutcomeOutput::Failed {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            message: "non-fast-forward".to_owned(),
        },
        PushOutcomeOutput::ProjectRepoPushed {
            path: "projects/proj".to_owned(),
            absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new(
                "/ws/projects/proj",
            )),
            project: repoweave::manifest::ProjectName::new("proj").unwrap(),
        },
        PushOutcomeOutput::ProjectRepoFailed {
            path: "projects/proj".to_owned(),
            absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new(
                "/ws/projects/proj",
            )),
            project: repoweave::manifest::ProjectName::new("proj").unwrap(),
            message: "remote rejected".to_owned(),
        },
    ]
}

fn push_corpus() -> Vec<(&'static str, Value)> {
    vec![
        (
            "every-outcome-kind",
            value(PushJsonOutput {
                schema_url: PUSH_SCHEMA_URL.to_owned(),
                outcomes: push_outcomes(),
                resolution: Some(unregistered_resolution()),
            }),
        ),
        (
            "unresolved-workspace",
            value(PushJsonOutput {
                schema_url: PUSH_SCHEMA_URL.to_owned(),
                outcomes: Vec::new(),
                resolution: None,
            }),
        ),
    ]
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

fn every_relation() -> Vec<LockRelation> {
    vec![
        LockRelation::Ok,
        LockRelation::Ahead,
        LockRelation::Behind,
        LockRelation::Diverged,
        LockRelation::NoLock,
        LockRelation::Unknown,
        LockRelation::Missing,
        LockRelation::Unreachable,
    ]
}

fn status_repos() -> Vec<RepoStatus> {
    every_relation()
        .into_iter()
        .enumerate()
        .map(|(i, relation)| {
            // Alternate the optional fields so neither the present nor the
            // absent wire shape goes unsampled.
            let sparse = i % 2 == 1;
            RepoStatus {
                path: REPO.to_owned(),
                branch: (!sparse).then(|| "main".to_owned()),
                tip: (!sparse).then(|| sha('a')),
                lock_sha: (!sparse).then(|| sha('b')),
                lock_pinned_by_name: sparse,
                relation,
                mid_op: MidOperation::all().nth(i / 2).filter(|_| sparse),
                role: repoweave::manifest::Role::ALL[i % repoweave::manifest::Role::ALL.len()],
                url: "https://github.com/acme/repo.git".to_owned(),
                project: repoweave::manifest::ProjectName::new("proj").unwrap(),
                absolute_path: wire(ABS),
                parent: (!sparse).then(|| ParentInfo {
                    path: repoweave::path_spelling::WirePath::of(std::path::Path::new("/ws")),
                    tip: Some(repoweave::vcs::CommitId::parse(&sha('c')).unwrap()),
                }),
            }
        })
        .collect()
}

fn op_status(verb: OpVerb, phase: OpPhase) -> OpStatus {
    OpStatus {
        id: "op-1".to_owned(),
        verb,
        phase,
        started_at: "2026-01-01T00:00:00Z".to_owned(),
        owner: wire("/ws"),
        source: wire("/ws/.workweaves/proj--feat-a"),
        target: wire("/ws"),
        overrides: vec![Override::AllowStaleLock, Override::DiscardLocalCommits],
    }
}

fn status_corpus() -> Vec<(&'static str, Value)> {
    let mut docs = vec![
        (
            "every-relation-no-op",
            value(StatusJsonOutput {
                schema_url: STATUS_SCHEMA_URL.to_owned(),
                repos: status_repos(),
                resolution: Some(resolution()),
                op: None,
            }),
        ),
        (
            "unresolved-workspace",
            value(StatusJsonOutput {
                schema_url: STATUS_SCHEMA_URL.to_owned(),
                repos: Vec::new(),
                resolution: None,
                op: None,
            }),
        ),
    ];
    // Every phase, and both verbs, since `op` is the one place those
    // vocabularies reach `rwv status --json`.
    for (label, verb, phase) in [
        ("op-sync-replay", OpVerb::Sync, OpPhase::Replay),
        ("op-sync-relock", OpVerb::Sync, OpPhase::Relock),
        (
            "op-sync-to-advance-target",
            OpVerb::SyncTo,
            OpPhase::AdvanceTarget,
        ),
        ("op-sync-to-retire", OpVerb::SyncTo, OpPhase::Retire),
    ] {
        docs.push((
            label,
            value(StatusJsonOutput {
                schema_url: STATUS_SCHEMA_URL.to_owned(),
                repos: Vec::new(),
                resolution: Some(primary_resolution()),
                op: Some(op_status(verb, phase)),
            }),
        ));
    }
    docs
}

// ---------------------------------------------------------------------------
// sync and sync-to
// ---------------------------------------------------------------------------

/// Every [`VcsErrorOutput`] variant, which is what a sync failure's `cause`
/// carries. Written as a list so a variant added to the enum leaves the list
/// short, and as an exhaustive match in [`vcs_error_tag`] so the same addition
/// stops this file compiling.
fn every_vcs_error() -> Vec<VcsErrorOutput> {
    let repo = wire(ABS);
    let mut errors = vec![
        VcsErrorOutput::NotARepo { path: repo.clone() },
        VcsErrorOutput::RevisionNotFound {
            repo: repo.clone(),
            rev: sha('a'),
        },
        VcsErrorOutput::RebaseConflict {
            repo: repo.clone(),
            op: ConflictOp::Rebase,
        },
        VcsErrorOutput::RebaseConflict {
            repo: repo.clone(),
            op: ConflictOp::Merge,
        },
        VcsErrorOutput::RebaseConflict {
            repo: repo.clone(),
            op: ConflictOp::CherryPick,
        },
        VcsErrorOutput::StaleRefWitness {
            repo: repo.clone(),
            expected: sha('a'),
            observed: sha('b'),
        },
        VcsErrorOutput::HookRejected {
            repo: repo.clone(),
            stderr: "pre-push refused".to_owned(),
        },
        VcsErrorOutput::UntrackedCollision {
            repo: repo.clone(),
            paths: vec!["src/main.rs".to_owned()],
        },
        VcsErrorOutput::Io {
            ctx: "read HEAD".to_owned(),
            message: "permission denied".to_owned(),
        },
        VcsErrorOutput::CommandFailed {
            args: vec!["rev-parse".to_owned(), "HEAD".to_owned()],
            repo: repo.clone(),
            stderr: "fatal".to_owned(),
        },
    ];
    errors.extend(
        MidOperation::all().map(|operation| VcsErrorOutput::MidOperation {
            repo: repo.clone(),
            operation,
        }),
    );
    errors
}

/// Exhaustive on purpose: a new `VcsErrorOutput` variant stops this file
/// compiling until whoever added it also adds a sample above.
fn vcs_error_tag(e: &VcsErrorOutput) -> &'static str {
    match e {
        VcsErrorOutput::NotARepo { .. } => "not-a-repo",
        VcsErrorOutput::RevisionNotFound { .. } => "revision-not-found",
        VcsErrorOutput::RebaseConflict { .. } => "rebase-conflict",
        VcsErrorOutput::StaleRefWitness { .. } => "stale-ref-witness",
        VcsErrorOutput::MidOperation { .. } => "mid-operation",
        VcsErrorOutput::HookRejected { .. } => "hook-rejected",
        VcsErrorOutput::UntrackedCollision { .. } => "untracked-collision",
        VcsErrorOutput::Io { .. } => "io",
        VcsErrorOutput::CommandFailed { .. } => "command-failed",
    }
}

/// Every [`SyncFailureOutput`] shape, `head-unreadable` both with and without
/// its optional `cause`, and every `cause` the wire can carry.
fn sync_failures() -> Vec<SyncFailureOutput> {
    let mut failures = vec![SyncFailureOutput::HeadUnreadable {
        message: "HEAD is unreadable".to_owned(),
        cause: None,
    }];
    for (n, cause) in every_vcs_error().into_iter().enumerate() {
        let message = format!("failed: {}", vcs_error_tag(&cause));
        failures.push(match n % 3 {
            0 => SyncFailureOutput::HeadUnreadable {
                message,
                cause: Some(cause),
            },
            1 => SyncFailureOutput::FfImpossible { message, cause },
            _ => SyncFailureOutput::RebaseFailed { message, cause },
        });
    }
    failures
}

/// Every [`SyncOutcomeOutput`] variant. `step3_advance` is sampled at both
/// settings on every variant that carries it, because `sync` omits it and
/// `sync-to` supplies it — one envelope's normal shape is the other's.
/// Every containment verdict, each paired with a baseline, so both closed
/// vocabularies are sampled across one corpus document. Absence is sampled
/// separately by the outcomes that carry `None`.
fn containments() -> Vec<ContainmentOutput> {
    vec![
        ContainmentOutput {
            verdict: ContainmentVerdictOutput::Behind { commits: 2 },
            baseline: ReplayBaseline::SourceLockEntry,
        },
        ContainmentOutput {
            verdict: ContainmentVerdictOutput::Diverged {
                ahead: 1,
                behind: 4,
            },
            baseline: ReplayBaseline::SourceCommittedTip,
        },
        ContainmentOutput {
            verdict: ContainmentVerdictOutput::Ahead { commits: 3 },
            baseline: ReplayBaseline::SourceLockEntry,
        },
        ContainmentOutput {
            verdict: ContainmentVerdictOutput::Equal,
            baseline: ReplayBaseline::SourceCommittedTip,
        },
    ]
}

fn sync_outcomes(with_step3: bool) -> Vec<SyncOutcomeOutput> {
    let step3 = with_step3.then(advance);
    let c = containments();
    let mut outcomes = vec![
        SyncOutcomeOutput::Converged {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            step3_advance: step3.clone(),
            derived_content_dropped: Vec::new(),
            containment: Some(c[0]),
        },
        SyncOutcomeOutput::Converged {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            step3_advance: step3.clone(),
            derived_content_dropped: vec!["Cargo.lock".to_owned()],
            containment: Some(c[1]),
        },
        SyncOutcomeOutput::AlreadyAhead {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            commits_ahead: 3,
            step3_advance: step3.clone(),
            containment: Some(c[2]),
            source_position: Some(ContainmentVerdictOutput::Ahead { commits: 5 }),
        },
        SyncOutcomeOutput::AlreadyAhead {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            commits_ahead: 0,
            step3_advance: step3.clone(),
            containment: None,
            source_position: None,
        },
        SyncOutcomeOutput::NoOp {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            step3_advance: step3.clone(),
            containment: Some(c[3]),
        },
    ];
    for (n, failure) in sync_failures().into_iter().enumerate() {
        outcomes.push(SyncOutcomeOutput::Failed {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            failure,
            step3_advance: step3.clone(),
            // A failure decided before any pair was read carries no verdict,
            // and that shape has to be sampled too.
            containment: (n > 0).then(|| c[n % c.len()]),
        });
    }
    outcomes
}

/// Every member kind, and a refused prune both with and without a token.
fn members() -> Vec<MemberOutput> {
    vec![
        MemberOutput::Kept {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
        },
        MemberOutput::Pruned {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
        },
        MemberOutput::PruneRefused {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            refusal: Some("dirty-checkout".to_owned()),
            message: "worktree has uncommitted changes".to_owned(),
        },
        MemberOutput::PruneRefused {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            refusal: None,
            message: "worktree remove failed".to_owned(),
        },
    ]
}

fn sync_corpus() -> Vec<(&'static str, Value)> {
    vec![
        (
            "every-outcome-with-advisory",
            value(SyncJsonOutput {
                schema: SYNC_JSON_SCHEMA_URL.to_owned(),
                outcomes: sync_outcomes(false),
                placements: placements(),
                members: members(),
                complete: true,
                advisories: advisories(),
                resolution: Some(resolution()),
            }),
        ),
        (
            "every-outcome-carrying-step3",
            value(SyncJsonOutput {
                schema: SYNC_JSON_SCHEMA_URL.to_owned(),
                outcomes: sync_outcomes(true),
                placements: Vec::new(),
                members: Vec::new(),
                complete: false,
                advisories: Vec::new(),
                resolution: None,
            }),
        ),
    ]
}

fn sync_to_corpus() -> Vec<(&'static str, Value)> {
    vec![
        (
            "from-a-workweave-advancing-the-project-repo",
            value(SyncToJsonOutput {
                schema: SYNC_TO_JSON_SCHEMA_URL.to_owned(),
                source_workweave: Some("feat-a".to_owned()),
                target: repoweave::path_spelling::WirePath::of(std::path::Path::new("/ws")),
                retired: true,
                // Fresh run: the absent shape of the `resumed` disclosure.
                resumed: None,
                outcomes: sync_outcomes(true),
                placements: placements(),
                members: members(),
                complete: true,
                project_repo_advance: Some(advance()),
                resolution: Some(resolution()),
            }),
        ),
        (
            "from-the-primary-weave-no-op-advance",
            value(SyncToJsonOutput {
                schema: SYNC_TO_JSON_SCHEMA_URL.to_owned(),
                source_workweave: None,
                target: repoweave::path_spelling::WirePath::of(std::path::Path::new("/ws")),
                retired: false,
                // Resumed at replay (the common interruption, a step-1
                // conflict): replay re-ran, so outcomes carries every repo.
                resumed: Some(OpPhase::Replay),
                outcomes: sync_outcomes(false),
                placements: Vec::new(),
                members: Vec::new(),
                complete: false,
                project_repo_advance: None,
                resolution: None,
            }),
        ),
        // The three post-replay resume shapes: replay had completed before the
        // interruption, so the resumed invocation re-entered downstream of it
        // and `outcomes` is empty by construction — the `resumed` value is
        // what tells a consumer the emptiness is a resume, not a zero-repo op.
        (
            "resumed-downstream-of-replay-at-relock",
            value(SyncToJsonOutput {
                schema: SYNC_TO_JSON_SCHEMA_URL.to_owned(),
                source_workweave: Some("feat-a".to_owned()),
                target: repoweave::path_spelling::WirePath::of(std::path::Path::new("/ws")),
                retired: false,
                resumed: Some(OpPhase::Relock),
                outcomes: Vec::new(),
                placements: Vec::new(),
                members: Vec::new(),
                complete: false,
                project_repo_advance: Some(advance()),
                resolution: Some(resolution()),
            }),
        ),
        (
            "resumed-mid-advance-target",
            value(SyncToJsonOutput {
                schema: SYNC_TO_JSON_SCHEMA_URL.to_owned(),
                source_workweave: Some("feat-a".to_owned()),
                target: repoweave::path_spelling::WirePath::of(std::path::Path::new("/ws")),
                retired: false,
                resumed: Some(OpPhase::AdvanceTarget),
                outcomes: Vec::new(),
                placements: placements(),
                members: members(),
                complete: false,
                project_repo_advance: None,
                resolution: Some(resolution()),
            }),
        ),
        (
            "resumed-at-retire-after-clearing-the-refusal",
            value(SyncToJsonOutput {
                schema: SYNC_TO_JSON_SCHEMA_URL.to_owned(),
                source_workweave: Some("feat-a".to_owned()),
                target: repoweave::path_spelling::WirePath::of(std::path::Path::new("/ws")),
                retired: true,
                resumed: Some(OpPhase::Retire),
                outcomes: Vec::new(),
                placements: Vec::new(),
                members: Vec::new(),
                complete: true,
                project_repo_advance: None,
                resolution: Some(resolution()),
            }),
        ),
    ]
}

// ---------------------------------------------------------------------------
// update
// ---------------------------------------------------------------------------

fn update_records() -> Vec<RepoUpdateRecord> {
    [
        UpdateKind::Updated,
        UpdateKind::UpToDate,
        UpdateKind::Failed,
    ]
    .into_iter()
    .map(|kind| {
        let (old_sha, new_sha, error) = match kind {
            UpdateKind::Updated => (Some(sha('a')), Some(sha('b')), None),
            UpdateKind::UpToDate => (Some(sha('a')), Some(sha('a')), None),
            UpdateKind::Failed => (None, None, Some("not a fast-forward".to_owned())),
        };
        RepoUpdateRecord {
            path: REPO.to_owned(),
            absolute_path: wire(ABS),
            branch: "main".to_owned(),
            kind,
            old_sha,
            new_sha,
            error,
        }
    })
    .collect()
}

fn update_corpus() -> Vec<(&'static str, Value)> {
    vec![
        (
            "every-kind",
            value(UpdateJsonOutput {
                schema_url: UPDATE_SCHEMA_URL.to_owned(),
                repos: update_records(),
                placements: placements(),
                resolution: Some(primary_resolution()),
                skipped: every_skip(),
            }),
        ),
        (
            "unresolved-workspace",
            value(UpdateJsonOutput {
                schema_url: UPDATE_SCHEMA_URL.to_owned(),
                repos: Vec::new(),
                placements: Vec::new(),
                resolution: None,
                skipped: Vec::new(),
            }),
        ),
    ]
}

// ---------------------------------------------------------------------------
// NDJSON record corpora
//
// Each record is serialized directly from its production struct (with a
// concrete lifetime) so the corpus cannot drift from the struct the verb
// actually serializes at runtime.
// ---------------------------------------------------------------------------

fn fetch_record_corpus() -> Vec<(&'static str, Value)> {
    fetch_outcomes()
        .into_iter()
        .enumerate()
        .map(|(i, outcome)| {
            let label: &'static str = match i {
                0 => "ok",
                1 => "skipped",
                _ => "failed",
            };
            let v = value(FetchOutcomeNdjsonRecord {
                schema: FETCH_RECORD_SCHEMA_URL,
                outcome: &outcome,
            });
            (label, v)
        })
        .collect()
}

fn push_record_corpus() -> Vec<(&'static str, Value)> {
    push_outcomes()
        .into_iter()
        .enumerate()
        .map(|(i, outcome)| {
            let label: &'static str = match i {
                0 => "pushed",
                1 => "skipped",
                2 => "failed",
                3 => "project-repo-pushed",
                _ => "project-repo-failed",
            };
            let v = value(PushOutcomeNdjsonRecord {
                schema: PUSH_RECORD_SCHEMA_URL,
                outcome: &outcome,
            });
            (label, v)
        })
        .collect()
}

fn update_record_corpus() -> Vec<(&'static str, Value)> {
    update_records()
        .into_iter()
        .enumerate()
        .map(|(i, record)| {
            let label: &'static str = match i {
                0 => "updated",
                1 => "up-to-date",
                _ => "failed",
            };
            let v = value(UpdateNdjsonRecord {
                schema: UPDATE_RECORD_SCHEMA_URL,
                record: &record,
            });
            (label, v)
        })
        .collect()
}

fn sync_record_corpus() -> Vec<(&'static str, Value)> {
    sync_outcomes(false)
        .into_iter()
        .enumerate()
        .map(|(i, outcome)| {
            let label: &'static str = match i {
                0 => "converged-no-derived-drop",
                1 => "converged-derived-drop",
                2 => "already-ahead-3",
                3 => "already-ahead-0",
                4 => "no-op",
                _ => "failed",
            };
            let v = value(SyncOutcomeNdjsonRecord {
                schema: SYNC_RECORD_SCHEMA_URL,
                outcome: &outcome,
            });
            (label, v)
        })
        .collect()
}

fn sync_to_record_corpus() -> Vec<(&'static str, Value)> {
    sync_outcomes(true)
        .into_iter()
        .enumerate()
        .map(|(i, outcome)| {
            let label: &'static str = match i {
                0 => "converged-with-step3",
                1 => "converged-derived-drop-step3",
                2 => "already-ahead-step3",
                3 => "already-ahead-0-step3",
                4 => "no-op-step3",
                _ => "failed-step3",
            };
            let v = value(SyncOutcomeNdjsonRecord {
                schema: SYNC_TO_RECORD_SCHEMA_URL,
                outcome: &outcome,
            });
            (label, v)
        })
        .collect()
}

fn member_record_corpus() -> Vec<(&'static str, Value)> {
    members()
        .iter()
        .zip(["kept", "pruned", "prune-refused", "prune-refused-untokened"])
        .map(|(member, label)| {
            let v = value(MemberNdjsonRecord {
                schema: MEMBER_RECORD_SCHEMA_URL,
                member,
            });
            (label, v)
        })
        .collect()
}

fn advance_record_corpus() -> Vec<(&'static str, Value)> {
    let manifest_repo = AdvanceOutput::Step3Advance {
        path: REPO.to_owned(),
        from_sha: sha('a'),
        to_sha: sha('b'),
    };
    let project_repo = AdvanceOutput::ProjectRepoAdvance {
        from_sha: sha('c'),
        to_sha: sha('d'),
    };
    [
        (manifest_repo, "manifest-repo"),
        (project_repo, "project-repo"),
    ]
    .iter()
    .map(|(advance, label)| {
        let v = value(AdvanceNdjsonRecord {
            schema: ADVANCE_RECORD_SCHEMA_URL,
            advance,
        });
        (*label, v)
    })
    .collect()
}

fn placement_record_corpus() -> Vec<(&'static str, Value)> {
    placements()
        .iter()
        .zip(["this-workspace", "the-target"])
        .map(|(placement, label)| {
            let v = value(PlacementNdjsonRecord {
                schema: PLACEMENT_RECORD_SCHEMA_URL,
                placement,
            });
            (label, v)
        })
        .collect()
}

fn each_outcomes() -> Vec<(&'static str, each_wire::Outcome)> {
    use each_wire::EachOutcomeStatus as S;
    use each_wire::Role as R;
    let roles = [R::Owned, R::Fork, R::Dependency, R::Reference];
    [
        ("ok", S::Ok, Some(0), None, Some(12)),
        ("failed-exit", S::Failed, Some(3), None, Some(40)),
        ("failed-signal", S::Failed, None, Some(9), Some(40)),
        ("failed-to-start", S::Failed, None, None, Some(1)),
        ("timed-out", S::TimedOut, None, Some(9), Some(1000)),
        ("not-run", S::NotRun, None, None, None),
        ("not-present", S::NotPresent, None, None, None),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (label, status, exit_code, signal, duration_ms))| {
        (
            label,
            each_wire::Outcome {
                path: "github/org/app".into(),
                role: roles[i % roles.len()],
                status,
                exit_code,
                signal,
                duration_ms,
                stdout: "out\n".into(),
                stderr: String::new(),
            },
        )
    })
    .collect()
}

fn each_corpus() -> Vec<(&'static str, Value)> {
    let outcomes: Vec<each_wire::Outcome> = each_outcomes().into_iter().map(|(_, o)| o).collect();
    vec![
        (
            "every-status",
            value(each_wire::EachJsonOutput {
                schema: each_schema_url::EACH_SCHEMA_URL.to_owned(),
                outcomes,
            }),
        ),
        (
            "empty",
            value(each_wire::EachJsonOutput {
                schema: each_schema_url::EACH_SCHEMA_URL.to_owned(),
                outcomes: Vec::new(),
            }),
        ),
    ]
}

fn each_record_corpus() -> Vec<(&'static str, Value)> {
    each_outcomes()
        .into_iter()
        .map(|(label, outcome)| {
            let v = value(each_wire::EachNdjsonRecord {
                schema: each_schema_url::EACH_RECORD_SCHEMA_URL,
                outcome: &outcome,
            });
            (label, v)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The pins
// ---------------------------------------------------------------------------

/// The check this file exists to make impossible to fail silently: a committed
/// artifact with nothing validated against it.
#[test]
fn every_committed_schema_has_a_registered_case() {
    let committed: BTreeSet<String> = json_schema::committed_verbs().into_iter().collect();
    let registered: BTreeSet<String> = cases().iter().map(|c| c.verb.to_owned()).collect();

    let unvalidated: Vec<&String> = committed.difference(&registered).collect();
    assert!(
        unvalidated.is_empty(),
        "committed under docs/reference/schemas/ with nothing validated against them: \
         {unvalidated:?} — add a VerbCase, or the artifact is a contract nothing keeps"
    );
    let phantom: Vec<&String> = registered.difference(&committed).collect();
    assert!(
        phantom.is_empty(),
        "registered here with no committed artifact: {phantom:?}"
    );
    assert_eq!(committed.len(), cases().len());
}

#[test]
fn every_corpus_document_validates_against_its_committed_schema() {
    for case in cases() {
        let schema = json_schema::committed_schema(case.verb);
        let docs = (case.corpus)();
        assert!(
            docs.len() >= 2,
            "{}: a one-document corpus samples one wire shape",
            case.verb
        );
        for (label, doc) in docs {
            let (errors, walk) = json_schema::conform(&doc, &schema);
            assert!(
                errors.is_empty(),
                "{} [{label}] does not satisfy {}:\n  {}",
                case.verb,
                json_schema::schema_path(case.verb),
                errors.join("\n  ")
            );
            // Non-vacuity: a traversal that stopped early reports the same
            // empty error list as a clean document.
            assert!(
                walk.properties_checked >= 2,
                "{} [{label}]: the walk never reached the envelope's properties: {walk:?}",
                case.verb
            );
        }
    }
}

/// An envelope field no document carries is unvalidated in its present shape,
/// and `skip_serializing_if` makes present and absent two different wire
/// shapes. This is what narrows doctor's exception to the struct-literal rule:
/// a new optional field on `DoctorJsonOutput` compiles here, but it cannot
/// reach the artifact and stay unsampled.
///
/// Top-level only. A nested optional field is a much larger space and this
/// would reject corpora that are reasonable; the envelope is where a field is
/// added.
///
/// Skipped for per-record schemas: those describe a single flat object, not
/// an array-wrapping envelope, and their top-level `properties` may not list
/// all wire fields (variant fields live inside `oneOf` branches).
#[test]
fn every_corpus_carries_every_envelope_field_its_schema_declares() {
    for case in cases() {
        if is_record_schema(case.verb) {
            continue;
        }
        let schema = json_schema::committed_schema(case.verb);
        let declared: BTreeSet<String> = schema["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{}: the artifact declares no properties", case.verb))
            .keys()
            .cloned()
            .collect();
        let mut observed = BTreeSet::new();
        for (_, doc) in (case.corpus)() {
            observed.extend(
                doc.as_object()
                    .expect("every envelope is an object")
                    .keys()
                    .cloned(),
            );
        }
        let unsampled: Vec<&String> = declared.difference(&observed).collect();
        assert!(
            unsampled.is_empty(),
            "{} declares envelope fields no document in its corpus carries, so their presence \
             on the wire is unvalidated: {unsampled:?}",
            case.verb
        );
    }
}

/// A corpus that never takes a shape the artifact permits validates and proves
/// nothing about that shape. The expectation is read out of the artifact so it
/// cannot fall behind it.
#[test]
fn every_corpus_exercises_every_member_its_schema_declares() {
    fn declares_a_wire_value(schema: &Value) -> bool {
        match schema {
            Value::Object(map) => {
                map.contains_key("enum")
                    || map.contains_key("const")
                    || map.values().any(declares_a_wire_value)
            }
            Value::Array(items) => items.iter().any(declares_a_wire_value),
            _ => false,
        }
    }
    for case in cases() {
        let schema = json_schema::committed_schema(case.verb);
        let declared = json_schema::declared_enum_values(&schema);
        if !declares_a_wire_value(&schema) {
            assert!(declared.is_empty(), "{}: {declared:?}", case.verb);
            continue;
        }
        assert!(
            !declared.is_empty(),
            "{}: no declared enum members were read — the coverage check is vacuous",
            case.verb
        );
        let mut observed = BTreeSet::new();
        for (_, doc) in (case.corpus)() {
            observed.extend(json_schema::observed_enum_values(&doc));
        }
        let unsampled: Vec<String> = declared
            .difference(&observed)
            .map(|(property, member)| format!("{property}: {member:?}"))
            .collect();
        assert!(
            unsampled.is_empty(),
            "{} declares wire values no document in its corpus takes, so conformance is \
             unmeasured for them: {unsampled:?}",
            case.verb
        );
    }
}

/// The committed artifacts must stay inside the validator's subset. This is
/// what turns "schemars emitted something new" into a failure here rather than
/// into silent under-validation of the real output.
///
/// Envelope schemas (e.g. `fetch.json`) wrap an array of per-repo records and
/// are expected to be moderately large; the minimum keyword count guards
/// against a truncated or empty schema slipping through. Per-record schemas
/// (e.g. `fetch-record.json`) describe a single flat object and are legitimately
/// smaller, so a proportionately lower minimum applies.
#[test]
fn every_committed_schema_stays_inside_the_validator_subset() {
    for verb in json_schema::committed_verbs() {
        let census = json_schema::census(&json_schema::committed_schema(&verb));
        // Per-record schemas describe one flat per-repo object (simpler than an
        // envelope wrapping an array). Apply a lower floor so the guard remains
        // meaningful without failing on correct, small schemas.
        let min_seen = if verb.ends_with("-record") { 10 } else { 20 };
        assert!(
            census.seen > min_seen,
            "{}: the keyword walk read almost nothing: {census:?}",
            json_schema::schema_path(&verb)
        );
        assert!(
            census.unknown.is_empty(),
            "{} uses keywords the validator does not implement, so emitted output is only \
             partly checked: {:?}",
            json_schema::schema_path(&verb),
            census.unknown
        );
    }
}

#[test]
fn every_envelope_embeds_the_url_of_the_artifact_validated_here() {
    for case in cases() {
        let suffix = format!("/{}", json_schema::schema_path(case.verb));
        assert!(
            case.schema_url.ends_with(&suffix),
            "{} embeds {} which does not name {}",
            case.verb,
            case.schema_url,
            json_schema::schema_path(case.verb)
        );
        for (label, doc) in (case.corpus)() {
            assert_eq!(
                doc["$schema"].as_str(),
                Some(case.schema_url),
                "{} [{label}] points a consumer at a different artifact than the one it was \
                 validated against",
                case.verb
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Seeded failures
// ---------------------------------------------------------------------------
//
// The divergences a `--json` surface actually takes, applied to each verb's
// own envelope: a renamed key, an added key, a dropped required key, a
// retyped leaf. Every one of these is a well-formed JSON document, and every
// one strands a consumer reading the artifact the bytes name.

fn first_doc(case: &VerbCase) -> Value {
    (case.corpus)()
        .into_iter()
        .next()
        .expect("every case has a corpus")
        .1
}

/// True for per-record NDJSON schema artifacts (e.g. `fetch-record`).
///
/// Record schemas describe one flat per-repo object rather than an envelope
/// wrapping an array of records. Envelope-specific tests (renamed array key,
/// dropped array field, etc.) do not apply to them and are skipped for these.
fn is_record_schema(verb: &str) -> bool {
    verb.ends_with("-record")
}

/// The array key each ENVELOPE carries its per-repo records under.
///
/// Only meaningful for envelope schemas; call sites gate on `!is_record_schema`
/// before using this.
fn records_key(verb: &str) -> &'static str {
    match verb {
        "doctor" => "violations",
        "status" | "update" => "repos",
        _ => "outcomes",
    }
}

#[test]
fn a_renamed_envelope_key_is_reported_for_every_verb() {
    for case in cases() {
        // Record schemas describe a flat per-repo object, not an envelope
        // wrapping an array. Renaming an "array key" has no meaning for them.
        if is_record_schema(case.verb) {
            continue;
        }
        let schema = json_schema::committed_schema(case.verb);
        let key = records_key(case.verb);
        let mut doc = first_doc(&case);
        let records = doc[key].take();
        let object = doc.as_object_mut().expect("the envelope is an object");
        object.remove(key);
        object.insert("findings".to_owned(), records);

        let (errors, _) = json_schema::conform(&doc, &schema);
        assert!(
            errors.iter().any(|e| e.contains(&format!("`{key}`"))),
            "{}: renaming `{key}` must be reported, got {errors:?}",
            case.verb
        );
        assert!(
            errors.iter().any(|e| e.contains("`findings`")),
            "{}: the renamed key must be reported as undeclared, got {errors:?}",
            case.verb
        );
    }
}

#[test]
fn an_added_envelope_key_is_reported_for_every_verb() {
    for case in cases() {
        let schema = json_schema::committed_schema(case.verb);
        let mut doc = first_doc(&case);
        doc.as_object_mut()
            .expect("the envelope is an object")
            .insert("scope".to_owned(), json!("all"));
        let (errors, _) = json_schema::conform(&doc, &schema);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("undeclared property `scope`")),
            "{}: a key the artifact does not declare must be reported, got {errors:?}",
            case.verb
        );
    }
}

#[test]
fn a_retyped_envelope_array_is_reported_for_every_verb() {
    for case in cases() {
        // Record schemas describe a flat per-repo object, not an envelope
        // wrapping an array. The "retype array" failure is envelope-specific.
        if is_record_schema(case.verb) {
            continue;
        }
        let schema = json_schema::committed_schema(case.verb);
        let key = records_key(case.verb);
        let mut doc = first_doc(&case);
        doc[key] = json!({ "count": 1 });
        let (errors, _) = json_schema::conform(&doc, &schema);
        assert!(
            errors.iter().any(|e| e.contains("expected type array")),
            "{}: `{key}` as an object must be reported, got {errors:?}",
            case.verb
        );
    }
}

#[test]
fn a_dropped_field_inside_a_record_is_reported_for_every_verb() {
    for case in cases() {
        // Record schemas describe a flat per-repo object. The record IS the
        // document (no outer envelope array to index into). Dropping a field
        // from the top level is tested via the record's own required constraint
        // in `every_corpus_document_validates_against_its_committed_schema`.
        if is_record_schema(case.verb) {
            continue;
        }
        let schema = json_schema::committed_schema(case.verb);
        let key = records_key(case.verb);
        let mut doc = first_doc(&case);
        let record = doc[key][0]
            .as_object_mut()
            .unwrap_or_else(|| panic!("{}: {key}[0] is an object", case.verb));
        let dropped = record
            .keys()
            .find(|k| k.as_str() != "kind")
            .expect("a record carries more than its tag")
            .clone();
        record.remove(&dropped);

        let (errors, _) = json_schema::conform(&doc, &schema);
        assert!(
            !errors.is_empty(),
            "{}: dropping `{dropped}` from {key}[0] was not reported",
            case.verb
        );
    }
}

/// A commit a doctor finding names is a commit id, so the empty string that
/// once stood for "none" is reported, as is an abbreviation; the one tip
/// that can be absent is `null`.
#[test]
fn a_doctor_commit_field_holds_a_full_commit_id_or_null() {
    let schema = json_schema::committed_schema("doctor");
    let case = cases()
        .into_iter()
        .find(|c| c.verb == "doctor")
        .expect("registered above");
    let doc = first_doc(&case);
    let with = |edit: &dyn Fn(&mut Value)| {
        let mut doc = doc.clone();
        let detached = doc["violations"]
            .as_array_mut()
            .expect("violations is an array")
            .iter_mut()
            .find_map(|v| v.get_mut("sub_kind")?.get_mut("detached"))
            .expect("the corpus carries a detached finding");
        edit(detached);
        json_schema::conform(&doc, &schema).0
    };

    let unedited = with(&|_| {});
    assert!(
        unedited.is_empty(),
        "the corpus document conforms: {unedited:?}"
    );
    for not_an_id in ["", "aaaaaaaaaaaa", "not-a-commit"] {
        assert!(
            !with(&|d| d["at_sha"] = json!(not_an_id)).is_empty(),
            "at_sha {not_an_id:?} was not reported"
        );
        assert!(
            !with(&|d| d["legacy_branch"]["tip_sha"] = json!(not_an_id)).is_empty(),
            "legacy tip_sha {not_an_id:?} was not reported"
        );
    }
    assert!(
        with(&|d| d["legacy_branch"]["tip_sha"] = Value::Null).is_empty(),
        "an absent legacy tip is null"
    );
    assert!(
        !with(&|d| d["at_sha"] = Value::Null).is_empty(),
        "HEAD always names a commit, so at_sha is never null"
    );
}

/// `commits_ahead` is the only place a numeric width reaches these artifacts,
/// and it is the one member `type: integer` alone does not pin: a negative
/// count is an integer. The pin is `format: uint` plus `minimum: 0`.
#[test]
fn a_negative_commit_count_is_reported() {
    for verb in ["sync", "sync-to"] {
        let schema = json_schema::committed_schema(verb);
        let case = cases()
            .into_iter()
            .find(|c| c.verb == verb)
            .expect("registered above");
        let mut doc = first_doc(&case);
        let outcomes = doc["outcomes"]
            .as_array_mut()
            .expect("outcomes is an array");
        let ahead = outcomes
            .iter_mut()
            .find(|o| o["kind"] == json!("already-ahead"))
            .expect("the corpus carries an already-ahead outcome");
        ahead["commits_ahead"] = json!(-1);

        let (errors, _) = json_schema::conform(&doc, &schema);
        assert!(
            errors.iter().any(|e| e.contains("format: uint")),
            "{verb}: a negative commits_ahead must be reported by `format`, got {errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("below minimum")),
            "{verb}: a negative commits_ahead must be reported by `minimum`, got {errors:?}"
        );
    }
}
